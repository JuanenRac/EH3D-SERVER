//! The spindle a machine carries and what it can hold. A machine has two spindles it can be fitted with: the one it leaves the factory with (`original`) and one with a
//! variable-frequency drive that the node of the machine drives (`vfd`). Each has limits: the slowest and the fastest turns, the fastest feed along the cut (XY) and of the plunge
//! (Z), the power and the thickest bit. The server keeps which one a machine carries and, with the safety switch on (the default), refuses a job that asks for more than that
//! spindle gives; with the switch on `warn` the job is accepted, the turns are cut to the top of the spindle and every excess is listed in the job.

use serde::{Deserialize, Serialize};

/// The spindles, by the word the API and the settings file use.
pub const IDS: [&str; 2] = ["original", "vfd"];

/// The limits of one spindle.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub min_rpm: u32,
    pub max_rpm: u32,
    pub power_w: u32,
    pub max_xy_mm_min: u32,
    pub max_z_mm_min: u32,
    pub max_bit_mm: f64,
}

/// What the person changed of a spindle's limits: only the members that are present replace the starting values.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Override {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_rpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_xy_mm_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_z_mm_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bit_mm: Option<f64>,
}

/// The starting limits of a spindle: prudent ones, the same the programs of the family start from; the machine's tuning replaces them.
pub fn defaults(id: &str) -> Option<Limits> {
    match id {
        "original" => Some(Limits {
            min_rpm: 10_000,
            max_rpm: 60_000,
            power_w: 200,
            max_xy_mm_min: 3_000,
            max_z_mm_min: 4_000,
            max_bit_mm: 3.2,
        }),
        "vfd" => Some(Limits {
            min_rpm: 6_000,
            max_rpm: 24_000,
            power_w: 2_200,
            max_xy_mm_min: 1_500,
            max_z_mm_min: 1_200,
            max_bit_mm: 6.0,
        }),
        _ => None,
    }
}

impl Override {
    /// The limits of spindle `id` with this change on top of the starting ones.
    pub fn over(&self, id: &str) -> Option<Limits> {
        let mut l = defaults(id)?;
        l.min_rpm = self.min_rpm.unwrap_or(l.min_rpm);
        l.max_rpm = self.max_rpm.unwrap_or(l.max_rpm);
        l.power_w = self.power_w.unwrap_or(l.power_w);
        l.max_xy_mm_min = self.max_xy_mm_min.unwrap_or(l.max_xy_mm_min);
        l.max_z_mm_min = self.max_z_mm_min.unwrap_or(l.max_z_mm_min);
        l.max_bit_mm = self.max_bit_mm.unwrap_or(l.max_bit_mm);
        Some(l)
    }
}

impl Limits {
    /// Whether the numbers can be used: nothing zero, the slowest not above the fastest, nothing absurd.
    pub fn valid(&self) -> bool {
        (1..=200_000).contains(&self.max_rpm)
            && self.min_rpm <= self.max_rpm
            && (1..=20_000).contains(&self.max_xy_mm_min)
            && (1..=20_000).contains(&self.max_z_mm_min)
            && (1..=20_000).contains(&self.power_w)
            && self.max_bit_mm.is_finite()
            && self.max_bit_mm > 0.0
            && self.max_bit_mm <= 50.0
    }
}

/// What the server does with a job that asks for more than the spindle gives. `Reject` is the starting value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Reject,
    Warn,
}

impl Mode {
    /// The word of the settings; an empty word is the conservative one.
    pub fn parse(word: &str) -> Option<Mode> {
        match word.trim() {
            "" | "reject" => Some(Mode::Reject),
            "warn" => Some(Mode::Warn),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Mode::Reject => "reject",
            Mode::Warn => "warn",
        }
    }
}

/// One excess: what, in which tool, the value asked for and the limit.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Finding {
    /// `rpm_above_max`, `rpm_below_min`, `feed_xy_above_max` or `feed_z_above_max`.
    pub code: String,
    pub tool: u8,
    pub value: u32,
    pub limit: u32,
}

/// The spindle a job is judged against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guard {
    pub limits: Limits,
    pub mode: Mode,
}

impl Guard {
    /// The excesses of one tool: the turns (0 means the tool leaves the spindle as it is, so there is nothing to judge) and the feeds (`None`: this kind of job has none).
    pub fn check(
        &self,
        tool: u8,
        rpm: u32,
        feed_xy: Option<u32>,
        feed_z: Option<u32>,
    ) -> Vec<Finding> {
        let mut found = Vec::new();
        let mut add = |code: &str, value: u32, limit: u32| {
            found.push(Finding {
                code: code.into(),
                tool,
                value,
                limit,
            })
        };
        if rpm > self.limits.max_rpm {
            add("rpm_above_max", rpm, self.limits.max_rpm);
        } else if rpm > 0 && rpm < self.limits.min_rpm {
            add("rpm_below_min", rpm, self.limits.min_rpm);
        }
        if let Some(f) = feed_xy.filter(|f| *f > self.limits.max_xy_mm_min) {
            add("feed_xy_above_max", f, self.limits.max_xy_mm_min);
        }
        if let Some(f) = feed_z.filter(|f| *f > self.limits.max_z_mm_min) {
            add("feed_z_above_max", f, self.limits.max_z_mm_min);
        }
        found
    }

    /// The turns the tool is sent with: with the switch on `warn` the turns never go above the top of the spindle.
    pub fn turns(&self, rpm: u32) -> u32 {
        match self.mode {
            Mode::Warn => rpm.min(self.limits.max_rpm),
            Mode::Reject => rpm,
        }
    }
}

/// The sentence that says what is wrong, for the refusal of a job.
pub fn describe(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| {
            let what = match f.code.as_str() {
                "rpm_above_max" => "turns above the top of the spindle",
                "rpm_below_min" => "turns below the slowest the spindle holds",
                "feed_xy_above_max" => "feed along the cut above what the spindle holds",
                _ => "plunge feed above what the spindle holds",
            };
            format!(
                "tool {}: {} ({} against {})",
                f.tool, what, f.value, f.limit
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(mode: Mode) -> Guard {
        Guard {
            limits: defaults("vfd").unwrap(),
            mode,
        }
    }

    #[test]
    fn the_starting_limits_are_those_of_the_two_spindles() {
        let (o, v) = (defaults("original").unwrap(), defaults("vfd").unwrap());
        assert_eq!((o.max_rpm, v.max_rpm), (60_000, 24_000));
        assert!(o.valid() && v.valid() && defaults("steam").is_none());
    }

    #[test]
    fn a_change_replaces_only_what_it_names() {
        let o = Override {
            max_xy_mm_min: Some(900),
            ..Override::default()
        };
        let l = o.over("vfd").unwrap();
        assert_eq!(
            (l.max_xy_mm_min, l.max_z_mm_min),
            (900, defaults("vfd").unwrap().max_z_mm_min)
        );
        assert!(o.over("steam").is_none());
    }

    #[test]
    fn limits_that_cannot_be_used_are_not_valid() {
        let mut l = defaults("vfd").unwrap();
        l.min_rpm = l.max_rpm + 1;
        assert!(!l.valid());
        let mut l = defaults("vfd").unwrap();
        l.max_z_mm_min = 0;
        assert!(!l.valid());
        let mut l = defaults("vfd").unwrap();
        l.max_bit_mm = f64::NAN;
        assert!(!l.valid());
    }

    #[test]
    fn the_excesses_of_a_tool_are_found_and_zero_turns_are_left_alone() {
        let g = guard(Mode::Reject);
        assert!(g.check(1, 24_000, Some(1_500), Some(1_200)).is_empty());
        assert!(g.check(1, 0, None, None).is_empty());
        let codes: Vec<String> = g
            .check(2, 60_000, Some(2_000), Some(1_500))
            .into_iter()
            .map(|f| f.code)
            .collect();
        assert_eq!(
            codes,
            ["rpm_above_max", "feed_xy_above_max", "feed_z_above_max"]
        );
        assert_eq!(g.check(3, 3_000, None, None)[0].code, "rpm_below_min");
    }

    #[test]
    fn with_warn_the_turns_are_cut_to_the_top_and_with_reject_they_are_not_touched() {
        assert_eq!(guard(Mode::Warn).turns(60_000), 24_000);
        assert_eq!(guard(Mode::Warn).turns(12_000), 12_000);
        assert_eq!(guard(Mode::Reject).turns(60_000), 60_000);
    }

    #[test]
    fn the_switch_defaults_to_reject_and_unknown_words_are_refused() {
        assert_eq!(Mode::parse(""), Some(Mode::Reject));
        assert_eq!(Mode::parse("warn"), Some(Mode::Warn));
        assert_eq!(Mode::parse("off"), None);
        assert_eq!(Mode::Warn.word(), "warn");
    }
}
