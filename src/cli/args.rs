//! ds-tool's command line: after the command, its words, and options with values.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A mistake in how a command is written, as against a problem with what it works on: the answer
/// to it shows how the command is written.
#[derive(Debug)]
pub struct Wrong(pub String);

impl fmt::Display for Wrong {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Wrong {}

/// An error that says how a command is written wrong.
pub fn wrong(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Wrong(message.into()))
}

/// What came after the command's name.
#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// The words that are not options, in order.
    pub words: Vec<String>,
    /// `--name value` or `--name=value`, by the name without its dashes.
    options: BTreeMap<String, String>,
    /// Options without a value, such as `--fit`.
    flags: BTreeSet<String>,
    /// `--json`: the answer as JSON.
    pub json: bool,
}

impl Args {
    /// Reads what came after the command, which may have the options `allowed`, and the options
    /// without a value `flags`. An option it does not know, one without its value, or a flag with one
    /// is an error that says so.
    pub fn parse(args: &[String], allowed: &[&str], flags: &[&str]) -> Result<Self> {
        let mut parsed = Self::default();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let Some(option) = arg.strip_prefix("--") else {
                parsed.words.push(arg.clone());
                continue;
            };
            if option == "json" {
                parsed.json = true;
                continue;
            }
            let (name, value) = option.split_once('=').map_or((option, None), |(name, value)| (name, Some(value)));
            if flags.contains(&name) {
                if value.is_some() {
                    return Err(wrong(format!("--{name} takes no value")));
                }
                parsed.flags.insert(name.to_owned());
                continue;
            }
            if !allowed.contains(&name) {
                return Err(wrong(format!("it has no --{name}")));
            }
            let value = match value {
                Some(value) => value.to_owned(),
                None => rest.next().cloned().ok_or_else(|| wrong(format!("--{name} needs a value after it")))?,
            };
            parsed.options.insert(name.to_owned(), value);
        }
        Ok(parsed)
    }

    /// The words, when there are as many as `names`; else an error that names the ones it takes.
    pub fn words<const N: usize>(&self, names: [&str; N]) -> Result<[&str; N]> {
        if self.words.len() != N {
            return Err(wrong(format!("it takes {}, and got {} word{}", names.join(" "), self.words.len(), if self.words.len() == 1 { "" } else { "s" })));
        }
        Ok(std::array::from_fn(|i| self.words[i].as_str()))
    }

    pub fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    /// Whether the option without a value `name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    /// An option that is a time, if it was given.
    pub fn time(&self, name: &str) -> Result<Option<f64>> {
        self.option(name).map(|text| parse_time(text).map_err(|error| wrong(format!("--{name}: {error}")))).transpose()
    }

    /// An option that is a clip's number, if it was given.
    pub fn number(&self, name: &str) -> Result<Option<usize>> {
        self.option(name).map(|text| parse_number(text).map_err(|error| wrong(format!("--{name}: {error}")))).transpose()
    }
}

/// A clip's number, as `show` lists them: 1 for the first.
pub fn parse_number(text: &str) -> Result<usize> {
    text.trim().parse::<usize>().ok().filter(|number| *number > 0).ok_or_else(|| wrong(format!("{text:?} is not a clip's number: they start at 1, as show lists them")))
}

/// A time as people write it: seconds (75 or 75.5), minutes and seconds (1:15 or 1:15.5), or hours,
/// minutes and seconds (1:02:03).
pub fn parse_time(text: &str) -> Result<f64> {
    seconds(text).ok_or_else(|| wrong(format!("{text:?} is not a time: write seconds, as 75.5, minutes and seconds, as 1:15.5, or hours too, as 1:02:03")))
}

fn seconds(text: &str) -> Option<f64> {
    let number = |part: &str| part.trim().parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0);
    match text.trim().split(':').collect::<Vec<_>>().as_slice() {
        [s] => number(s),
        [m, s] => Some(number(m)? * 60.0 + number(s)?),
        [h, m, s] => Some((number(h)? * 60.0 + number(m)?) * 60.0 + number(s)?),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn words_and_options_in_either_form() {
        let args = Args::parse(&words("lesson clip.mp4 --from 0:12 --fit --to=4:05 --json"), &["from", "to"], &["fit"]).unwrap();
        assert_eq!(args.words, ["lesson", "clip.mp4"]);
        assert_eq!((args.option("from"), args.option("to"), args.json), (Some("0:12"), Some("4:05"), true));
        assert_eq!(args.words(["<project>", "<file>"]).unwrap(), ["lesson", "clip.mp4"]);
        assert_eq!(args.time("to").unwrap(), Some(245.0));
        assert_eq!(args.time("name").unwrap(), None);
        assert!(args.flag("fit") && !args.flag("json"));
        let plain = Args::parse(&words("lesson clip.mp4"), &[], &["fit"]).unwrap();
        assert!(!plain.flag("fit"));
    }

    #[test]
    fn what_is_wrong_on_the_line_is_said() {
        let error = |text: &str, allowed: &[&str]| Args::parse(&words(text), allowed, &["fit"]).unwrap_err().to_string();
        assert_eq!(error("lesson --form 1", &["from"]), "it has no --form");
        assert_eq!(error("lesson --from", &["from"]), "--from needs a value after it");
        assert_eq!(error("lesson --fit=yes", &[]), "--fit takes no value");
        let args = Args::parse(&words("lesson"), &[], &[]).unwrap();
        assert_eq!(args.words(["<project>", "<file>"]).unwrap_err().to_string(), "it takes <project> <file>, and got 1 word");
        let args = Args::parse(&words("lesson --before 0 --to x"), &["before", "to"], &[]).unwrap();
        assert!(args.number("before").unwrap_err().to_string().starts_with("--before: \"0\" is not a clip's number"));
        assert!(args.time("to").unwrap_err().to_string().starts_with("--to: \"x\" is not a time"));
    }

    #[test]
    fn times_as_people_write_them() {
        let time = |text: &str| parse_time(text).ok();
        assert_eq!(time("75"), Some(75.0));
        assert_eq!(time("75.5"), Some(75.5));
        assert_eq!(time("1:15"), Some(75.0));
        assert_eq!(time("1:15.5"), Some(75.5));
        assert_eq!(time("0:04:05"), Some(245.0));
        assert_eq!(time("1:02:03"), Some(3723.0));
        for wrong in ["", "x", "-1", "1:2:3:4", "1::5", "inf"] {
            assert_eq!(time(wrong), None, "{wrong:?}");
        }
        assert_eq!(parse_number("3").unwrap(), 3);
        assert!(parse_number("0").is_err() && parse_number("two").is_err());
    }
}
