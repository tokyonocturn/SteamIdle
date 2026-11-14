//! Launch-queue scheduler.
//!
//! A queue is an ordered list of installed games with a dwell time. The worker
//! calls [`QueueRunner::tick`] on every poll; the runner only *decides* what to
//! do, which keeps the state machine deterministic and unit-testable. The
//! worker performs the actual Steam launches and process closing.
//!
//! `QueueRunner` never fakes playtime and never talks to Steam's servers.

use crate::engine::models::QueueItem;
use std::collections::HashMap;

/// Seconds to wait for a game process to appear after asking Steam to launch.
const LAUNCH_TIMEOUT_SECS: i64 = 120;
/// Grace period between "close the game" and forcing it closed.
const CLOSE_GRACE_SECS: i64 = 15;
/// Pause between queued games so Steam can settle.
const GAP_SECS: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    WaitingStart,
    WaitingForGame,
    Playing,
    Cooldown,
    Finished,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::WaitingStart => "starting",
            Self::WaitingForGame => "waiting for game",
            Self::Playing => "playing",
            Self::Cooldown => "closing game",
            Self::Finished => "finished",
        }
    }
}

/// What the worker must do with the operating system after a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Launch { appid: u32 },
    Close { pids: Vec<u32> },
    Kill { pids: Vec<u32> },
}

/// The public, cloneable view the dashboard renders.
#[derive(Debug, Clone)]
pub struct QueueRunView {
    pub name: String,
    pub index: usize,
    pub total: usize,
    pub game_name: String,
    pub phase: Phase,
    pub message: String,
    pub remaining_secs: i64,
}

pub struct QueueRunner {
    pub queue_id: i64,
    pub name: String,
    pub items: Vec<QueueItem>,
    pub index: usize,
    pub phase: Phase,
    deadline: i64,
    action_at: i64,
    closing_pids: Vec<u32>,
    message: String,
}

impl QueueRunner {
    pub fn new(queue_id: i64, name: String, items: Vec<QueueItem>, now: i64) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        Some(Self {
            queue_id,
            name,
            items,
            index: 0,
            phase: Phase::WaitingStart,
            deadline: now,
            action_at: now + 1,
            closing_pids: Vec::new(),
            message: "Queue ready".to_string(),
        })
    }

    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Finished
    }

    /// AppID of the game this queue is currently handling.
    pub fn current_appid(&self) -> Option<u32> {
        self.items.get(self.index).map(|item| item.appid)
    }

    pub fn snapshot(&self, now: i64) -> QueueRunView {
        let game_name = self
            .items
            .get(self.index)
            .map(|item| item.game_name.clone())
            .unwrap_or_default();
        QueueRunView {
            name: self.name.clone(),
            index: self.index,
            total: self.items.len(),
            game_name,
            phase: self.phase,
            message: self.message.clone(),
            remaining_secs: if self.phase == Phase::Playing {
                (self.deadline - now).max(0)
            } else {
                0
            },
        }
    }

    /// Advance the schedule. `running` maps AppIDs to their live process IDs.
    pub fn tick(&mut self, now: i64, running: &HashMap<u32, Vec<u32>>) -> Vec<Action> {
        let mut actions = Vec::new();
        match self.phase {
            Phase::WaitingStart => {
                if now < self.action_at {
                    return actions;
                }
                let Some(item) = self.items.get(self.index) else {
                    self.phase = Phase::Finished;
                    return actions;
                };
                self.phase = Phase::WaitingForGame;
                self.deadline = now + LAUNCH_TIMEOUT_SECS;
                self.message = format!("Launching {} via Steam", item.game_name);
                actions.push(Action::Launch { appid: item.appid });
            }
            Phase::WaitingForGame => {
                let Some(item) = self.items.get(self.index) else {
                    self.phase = Phase::Finished;
                    return actions;
                };
                if running.get(&item.appid).is_some_and(|pids| !pids.is_empty()) {
                    self.phase = Phase::Playing;
                    self.deadline = now + item.minutes.max(1) as i64 * 60;
                    self.message = format!("{} is running for {} min", item.game_name, item.minutes);
                } else if now >= self.deadline {
                    self.message = format!("{} did not start in time; skipping", item.game_name);
                    self.advance(now);
                }
            }
            Phase::Playing => {
                let Some(item) = self.items.get(self.index) else {
                    self.phase = Phase::Finished;
                    return actions;
                };
                let pids = running.get(&item.appid).cloned().unwrap_or_default();
                if pids.is_empty() {
                    self.message = format!("{} closed; moving on", item.game_name);
                    self.advance(now);
                } else if now >= self.deadline {
                    self.closing_pids = pids;
                    self.phase = Phase::Cooldown;
                    self.action_at = now + CLOSE_GRACE_SECS;
                    self.message = format!("{} slot finished; closing the game", item.game_name);
                    actions.push(Action::Close {
                        pids: self.closing_pids.clone(),
                    });
                }
            }
            Phase::Cooldown => {
                let Some(item) = self.items.get(self.index) else {
                    self.phase = Phase::Finished;
                    return actions;
                };
                let pids = running.get(&item.appid).cloned().unwrap_or_default();
                if pids.is_empty() {
                    self.message = "Game closed".to_string();
                    self.advance(now);
                } else if now >= self.action_at {
                    self.message = "Game ignored the close request; ending process".to_string();
                    actions.push(Action::Kill { pids });
                    self.advance(now);
                }
            }
            Phase::Finished => {}
        }
        actions
    }

    fn advance(&mut self, now: i64) {
        self.index += 1;
        self.closing_pids.clear();
        if self.index >= self.items.len() {
            self.phase = Phase::Finished;
            self.message = "Queue complete".to_string();
        } else {
            self.phase = Phase::WaitingStart;
            self.action_at = now + GAP_SECS;
            self.message = "Waiting for the next slot".to_string();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn item(appid: u32, name: &str, minutes: u32) -> QueueItem {
        QueueItem {
            id: appid as i64,
            appid,
            game_name: name.to_string(),
            minutes,
        }
    }

    fn running(appid: u32) -> HashMap<u32, Vec<u32>> {
        let mut map = HashMap::new();
        map.insert(appid, vec![1234]);
        map
    }

    fn empty() -> HashMap<u32, Vec<u32>> {
        HashMap::new()
    }

    #[test]
    fn empty_queue_is_rejected() {
        assert!(QueueRunner::new(1, "empty".into(), vec![], 0).is_none());
    }

    #[test]
    fn full_cycle_launches_plays_closes_and_finishes() {
        let mut runner =
            QueueRunner::new(1, "test".into(), vec![item(440, "TF2", 10)], 1_000).unwrap();

        // Phase 1: after the tiny start delay, the runner asks for a launch.
        let actions = runner.tick(1_001, &empty());
        assert_eq!(actions, vec![Action::Launch { appid: 440 }]);

        // Phase 2: game appears.
        let actions = runner.tick(1_010, &running(440));
        assert!(actions.is_empty());
        assert_eq!(runner.phase, Phase::Playing);

        // Phase 3: still inside the dwell time, nothing happens.
        assert!(runner.tick(1_010 + 60, &running(440)).is_empty());
        assert_eq!(runner.phase, Phase::Playing);

        // Phase 4: dwell time over -> graceful close.
        let actions = runner.tick(1_010 + 600, &running(440));
        assert_eq!(actions, vec![Action::Close { pids: vec![1234] }]);
        assert_eq!(runner.phase, Phase::Cooldown);

        // Phase 5: game still alive after the grace period -> force close.
        let actions = runner.tick(1_010 + 620, &running(440));
        assert_eq!(actions, vec![Action::Kill { pids: vec![1234] }]);
        assert_eq!(runner.phase, Phase::Finished);
        assert!(runner.is_finished());
    }

    #[test]
    fn game_closing_early_advances_to_next_item() {
        let mut runner = QueueRunner::new(
            1,
            "test".into(),
            vec![item(440, "TF2", 10), item(730, "CS2", 5)],
            0,
        )
        .unwrap();
        runner.tick(1, &empty());
        runner.tick(10, &running(440));
        assert_eq!(runner.index, 0);

        // User quits the game before the timer: move on after the gap.
        runner.tick(20, &empty());
        assert_eq!(runner.index, 1);
        assert_eq!(runner.phase, Phase::WaitingStart);

        runner.tick(30, &empty());
        assert_eq!(runner.phase, Phase::WaitingForGame);

        runner.tick(40, &running(730));
        assert_eq!(runner.phase, Phase::Playing);
        runner.tick(400, &running(730));
        assert_eq!(runner.phase, Phase::Cooldown);
        runner.tick(420, &empty());
        assert_eq!(runner.phase, Phase::Finished);
    }

    #[test]
    fn launch_timeout_skips_to_next_game() {
        let mut runner =
            QueueRunner::new(1, "test".into(), vec![item(440, "TF2", 5), item(730, "CS2", 5)], 0)
                .unwrap();
        runner.tick(1, &empty());
        // Game never shows up.
        runner.tick(1 + LAUNCH_TIMEOUT_SECS + 1, &empty());
        assert_eq!(runner.index, 1);
    }

    #[test]
    fn running_game_that_did_not_start_is_reported() {
        let mut runner = QueueRunner::new(1, "test".into(), vec![item(440, "TF2", 5)], 0).unwrap();
        runner.tick(1, &empty());
        runner.tick(5, &running(999));
        assert_eq!(runner.phase, Phase::WaitingForGame);
    }
}
