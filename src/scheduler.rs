//! PULSE + CRON scheduler (design doc §6.5). Ticks every 60s, reads the
//! agent-editable workspace files CRONS.json and PULSE.json, and fires tasks
//! as agent queries into a persistent "⏰ Scheduled" conversation — so
//! results are visible in chat history with no push infrastructure needed.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::agent::AgentRequest;
use crate::AppState;

const TICK: Duration = Duration::from_secs(60);
const CRONS_FILE: &str = "CRONS.json";
const PULSE_FILE: &str = "PULSE.json";
const SCHEDULER_CONVERSATION_KEY: &str = "scheduler_conversation_id";
const LAST_PULSE_KEY: &str = "last_pulse_at";
const SCHEDULED_CONVERSATION_TITLE: &str = "⏰ Scheduled";

const PULSE_PROMPT: &str = "This is your scheduled pulse — no one sent this message. Review your \
memory files and daily notes, check on anything you're tracking, and take any useful proactive \
action (update notes, tidy an app, prepare something for your human). If nothing needs attention, \
reply in one short sentence.";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub id: String,
    pub schedule: String,
    pub task: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub one_shot: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulseConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_pulse_interval")]
    pub interval_minutes: u64,
    #[serde(default)]
    pub quiet_hours: Option<QuietHours>,
}

fn default_pulse_interval() -> u64 {
    60
}

#[derive(Debug, Deserialize)]
pub struct QuietHours {
    pub start: String,
    pub end: String,
}

pub fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the immediate first tick so boot doesn't double-fire.
        interval.tick().await;
        // cron id -> last minute it fired ("YYYY-MM-DD HH:MM")
        let mut last_fired: HashMap<String, String> = HashMap::new();
        loop {
            interval.tick().await;
            let now = Local::now();
            run_crons(&state, now, &mut last_fired).await;
            run_pulse(&state, now).await;
        }
    });
}

async fn run_crons(state: &AppState, now: DateTime<Local>, last_fired: &mut HashMap<String, String>) {
    let path = state.workspace_dir.join(CRONS_FILE);
    let jobs = match read_crons(&path) {
        Ok(jobs) => jobs,
        Err(err) => {
            warn!("could not read {CRONS_FILE}: {err:#}");
            return;
        }
    };
    let minute_key = now.format("%Y-%m-%d %H:%M").to_string();
    let mut spent_one_shots: Vec<String> = Vec::new();

    for job in &jobs {
        if !job.enabled {
            continue;
        }
        if !cron_matches(&job.schedule, now) {
            continue;
        }
        if last_fired.get(&job.id).is_some_and(|m| m == &minute_key) {
            continue;
        }
        last_fired.insert(job.id.clone(), minute_key.clone());
        info!("cron {} fired: {}", job.id, job.task);
        fire(state, &format!("[scheduled task \"{}\"] {}", job.id, job.task)).await;
        if job.one_shot {
            spent_one_shots.push(job.id.clone());
        }
    }

    if !spent_one_shots.is_empty() {
        let remaining: Vec<&CronJob> = jobs
            .iter()
            .filter(|job| !spent_one_shots.contains(&job.id))
            .collect();
        match serde_json::to_string_pretty(&remaining) {
            Ok(json) => {
                if let Err(err) = std::fs::write(&path, json + "\n") {
                    warn!("could not remove spent one-shot crons: {err}");
                }
            }
            Err(err) => warn!("could not serialize crons: {err}"),
        }
    }
}

async fn run_pulse(state: &AppState, now: DateTime<Local>) {
    let path = state.workspace_dir.join(PULSE_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else { return };
    let pulse: PulseConfig = match serde_json::from_str(&raw) {
        Ok(pulse) => pulse,
        Err(err) => {
            warn!("could not parse {PULSE_FILE}: {err}");
            return;
        }
    };
    if !pulse.enabled {
        return;
    }
    if let Some(quiet) = &pulse.quiet_hours {
        if in_quiet_hours(quiet, now.time()) {
            return;
        }
    }
    let last = state
        .db
        .get_setting(LAST_PULSE_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let interval_secs = (pulse.interval_minutes.max(1) as i64) * 60;
    if now.timestamp() - last < interval_secs {
        return;
    }
    if let Err(err) = state.db.set_setting(LAST_PULSE_KEY, &now.timestamp().to_string()) {
        warn!("could not persist pulse timestamp: {err:#}");
        return;
    }
    info!("pulse fired");
    fire(state, PULSE_PROMPT).await;
}

/// Record the prompt in the scheduled conversation and hand it to the agent.
async fn fire(state: &AppState, prompt: &str) {
    let conversation_id = match scheduled_conversation(state) {
        Ok(id) => id,
        Err(err) => {
            warn!("no scheduler conversation: {err:#}");
            return;
        }
    };
    if let Err(err) = state.db.append_message(conversation_id, "scheduled", prompt) {
        warn!("could not record scheduled prompt: {err:#}");
    }
    let session_id = state.db.conversation_session(conversation_id).ok().flatten();
    let request = AgentRequest::Query {
        id: conversation_id.to_string(),
        prompt: prompt.to_string(),
        session_id,
        model: crate::api::effective_model(&state.db, conversation_id),
    };
    if let Err(err) = state.agent.send(request).await {
        warn!("could not queue scheduled query: {err:#}");
    }
}

/// Find-or-create the persistent "⏰ Scheduled" conversation.
fn scheduled_conversation(state: &AppState) -> anyhow::Result<i64> {
    if let Some(saved) = state.db.get_setting(SCHEDULER_CONVERSATION_KEY)? {
        if let Ok(id) = saved.parse::<i64>() {
            // The user may have deleted it; verify it still exists.
            if state.db.conversation_session(id).is_ok() {
                return Ok(id);
            }
        }
    }
    let id = state.db.create_conversation(SCHEDULED_CONVERSATION_TITLE)?;
    state
        .db
        .set_setting(SCHEDULER_CONVERSATION_KEY, &id.to_string())?;
    Ok(id)
}

fn read_crons(path: &Path) -> anyhow::Result<Vec<CronJob>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&raw)?)
}

/// Standard 5-field cron (minute hour dom month dow), local time.
/// Malformed expressions never match (and are logged once per tick).
fn cron_matches(expression: &str, now: DateTime<Local>) -> bool {
    // Note: croner's FromStr does NOT parse the pattern; Cron::new + parse() does.
    match croner::Cron::new(expression).parse() {
        Ok(cron) => {
            // Compare at minute granularity — our tick is 60s.
            let minute = now.with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap_or(now);
            cron.is_time_matching(&minute).unwrap_or(false)
        }
        Err(err) => {
            warn!("bad cron expression {expression:?}: {err}");
            false
        }
    }
}

fn in_quiet_hours(quiet: &QuietHours, time: NaiveTime) -> bool {
    let Ok(start) = NaiveTime::parse_from_str(&quiet.start, "%H:%M") else {
        return false;
    };
    let Ok(end) = NaiveTime::parse_from_str(&quiet.end, "%H:%M") else {
        return false;
    };
    if start <= end {
        time >= start && time < end
    } else {
        // spans midnight, e.g. 23:00–07:00
        time >= start || time < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 7, 7, hour, minute, 0).unwrap()
    }

    #[test]
    fn cron_matching_five_field() {
        assert!(cron_matches("* * * * *", at(9, 30)));
        assert!(cron_matches("30 9 * * *", at(9, 30)));
        assert!(!cron_matches("30 9 * * *", at(9, 31)));
        assert!(cron_matches("*/15 * * * *", at(9, 45)));
        assert!(!cron_matches("*/15 * * * *", at(9, 50)));
        assert!(!cron_matches("not a cron", at(9, 30)));
    }

    #[test]
    fn quiet_hours_same_day_and_spanning_midnight() {
        let same_day = QuietHours { start: "13:00".into(), end: "14:00".into() };
        assert!(in_quiet_hours(&same_day, NaiveTime::from_hms_opt(13, 30, 0).unwrap()));
        assert!(!in_quiet_hours(&same_day, NaiveTime::from_hms_opt(14, 0, 0).unwrap()));

        let overnight = QuietHours { start: "23:00".into(), end: "07:00".into() };
        assert!(in_quiet_hours(&overnight, NaiveTime::from_hms_opt(23, 30, 0).unwrap()));
        assert!(in_quiet_hours(&overnight, NaiveTime::from_hms_opt(3, 0, 0).unwrap()));
        assert!(!in_quiet_hours(&overnight, NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
        assert!(!in_quiet_hours(&overnight, NaiveTime::from_hms_opt(7, 0, 0).unwrap()));
    }

    #[test]
    fn crons_parse_with_defaults_and_tolerate_missing_file() {
        let jobs: Vec<CronJob> = serde_json::from_str(
            r#"[{"id":"a","schedule":"0 9 * * *","task":"morning summary"},
                {"id":"b","schedule":"* * * * *","task":"x","enabled":false,"oneShot":true}]"#,
        )
        .unwrap();
        assert!(jobs[0].enabled && !jobs[0].one_shot);
        assert!(!jobs[1].enabled && jobs[1].one_shot);

        assert!(read_crons(Path::new("/nonexistent/CRONS.json")).unwrap().is_empty());
    }
}
