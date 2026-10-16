//! Help in the user's language for the command-line tools.
//!
//! clap writes some help text itself: the section headings ("Usage:",
//! "Commands:", "Arguments:", "Options:"), the `--help` and `--version` flags
//! and the `help` subcommand. [`localize`] swaps these for the catalog's `[cli]`
//! sentences and lays the sections out as clap's default template does, so the
//! English help is byte for byte what clap derives. The tools set their own
//! abouts and argument helps from their tables before calling it, and test
//! the result with [`verify`].

use std::ffi::OsStr;

use clap::builder::{EnumValueParser, PossibleValue, TypedValueParser};
use clap::{Arg, Command, ValueEnum};
use ssr_types::app::Language;

use crate::catalog::cli;

const HELP: &str = "help";
const VERSION: &str = "version";
const HELP_SUBCOMMAND_ARG: &str = "subcommand";

/// Builds `command` and localizes clap's own help text through the tree.
pub fn localize(mut command: Command, language: Language) -> Command {
    command.build();
    localize_built(command, language)
}

fn localize_built(mut command: Command, language: Language) -> Command {
    if has_argument(&command, HELP) {
        command = command.mut_arg(HELP, |arg| {
            // clap points between the two helps when the command has a long one.
            if arg.get_long_help().is_some() {
                arg.help(cli::help_description_more(language))
                    .long_help(cli::help_description_summary(language))
            } else {
                arg.help(cli::help_description(language))
            }
        });
    }
    if has_argument(&command, VERSION) {
        command = command.mut_arg(VERSION, |arg| arg.help(cli::version_description(language)));
    }
    if language != Language::En {
        command = value_notes(command, language);
    }
    let template = template(&command, language);
    command = command.help_template(template);
    let names: Vec<String> = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_owned())
        .collect();
    for name in names {
        command = command.mut_subcommand(&name, |mut subcommand| {
            if name == HELP {
                subcommand = subcommand.about(cli::help_subcommand_about(language));
                if has_argument(&subcommand, HELP_SUBCOMMAND_ARG) {
                    subcommand = subcommand.mut_arg(HELP_SUBCOMMAND_ARG, |arg| {
                        arg.help(cli::help_subcommand_arg(language))
                    });
                }
            }
            localize_built(subcommand, language)
        });
    }
    command
}

/// A value parser for `E` whose possible values carry `help`'s sentences, so
/// `--help` describes them in the user's language. It parses as clap's own
/// [`EnumValueParser`], so `from_arg_matches` reads the same `E`.
#[derive(Clone)]
pub struct Values<E: ValueEnum + Clone + Send + Sync + 'static> {
    parser: EnumValueParser<E>,
    values: Vec<PossibleValue>,
}

impl<E: ValueEnum + Clone + Send + Sync + 'static> Values<E> {
    pub fn new(help: impl Fn(&E) -> &'static str) -> Self {
        let values = E::value_variants()
            .iter()
            .filter_map(|variant| Some(variant.to_possible_value()?.help(help(variant))))
            .collect();
        Self {
            parser: EnumValueParser::new(),
            values,
        }
    }
}

impl<E: ValueEnum + Clone + Send + Sync + 'static> std::fmt::Debug for Values<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Values")
            .field("values", &self.values)
            .finish_non_exhaustive()
    }
}

impl<E: ValueEnum + Clone + Send + Sync + 'static> TypedValueParser for Values<E> {
    type Value = E;

    fn parse_ref(
        &self,
        command: &Command,
        arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<E, clap::Error> {
        self.parser.parse_ref(command, arg, value)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(self.values.iter().cloned()))
    }
}

/// clap writes an argument's default and possible values in English
/// ("[default: …]", "[possible values: …]", "Possible values:"). In another
/// language they are hidden and the catalog's notes follow the help instead,
/// laid out as clap lays them out.
fn value_notes(mut command: Command, language: Language) -> Command {
    // In declaration order: mut_arg moves an argument to the end.
    let ids: Vec<String> = command
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect();
    for id in ids {
        command = command.mut_arg(id, |arg| {
            let takes_values = arg.get_action().takes_values();
            let mut notes = Vec::new();
            if takes_values
                && !arg.is_hide_default_value_set()
                && !arg.get_default_values().is_empty()
            {
                let values = arg
                    .get_default_values()
                    .iter()
                    .map(|value| {
                        let value = value.to_string_lossy();
                        if value.contains(char::is_whitespace) {
                            format!("{value:?}")
                        } else {
                            value.into_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                notes.push(cli::default_value(language, values));
            }
            let values: Vec<PossibleValue> = if arg.is_hide_possible_values_set() {
                Vec::new()
            } else {
                arg.get_possible_values()
                    .into_iter()
                    .filter(|value| !value.is_hide_set())
                    .collect()
            };
            if !values.is_empty() {
                let names = values
                    .iter()
                    .map(PossibleValue::get_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                notes.push(cli::possible_values(language, names));
            }
            if notes.is_empty() {
                return arg;
            }
            let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
            let long = arg.get_long_help().map(ToString::to_string);
            let listed = values.iter().any(|value| value.get_help().is_some());
            let mut arg = arg.hide_default_value(true).hide_possible_values(true);
            arg = arg.help(join(&help, &notes, " "));
            if listed {
                // Long help lists each value with its help on its own line, as
                // clap's "Possible values:" does; the short note stays for -h.
                let width = values
                    .iter()
                    .map(|value| value.get_name().chars().count())
                    .max()
                    .unwrap_or(0);
                let mut list = String::from(cli::possible_values_heading(language));
                for value in &values {
                    let name = value.get_name();
                    match value.get_help() {
                        Some(help) => {
                            let pad = width - name.chars().count();
                            list.push_str(&format!("\n- {name}: {:pad$}{help}", ""));
                        }
                        None => list.push_str(&format!("\n- {name}")),
                    }
                }
                let base = long.unwrap_or(help);
                let defaults: Vec<String> = notes[..notes.len() - 1].to_vec();
                arg = arg.long_help(format!("{}\n\n{list}", join(&base, &defaults, "\n")));
            } else if let Some(long) = long {
                arg = arg.long_help(join(&long, &notes, "\n"));
            }
            arg
        });
    }
    command
}

fn join(text: &str, notes: &[String], connector: &str) -> String {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(text.to_owned());
    }
    parts.extend(notes.iter().cloned());
    parts.join(connector)
}

fn has_argument(command: &Command, id: &str) -> bool {
    command.get_arguments().any(|arg| arg.get_id() == id)
}

/// clap's default template with the catalog's headings: the sections a
/// command has, in clap's order, a blank line apart.
fn template(command: &Command, language: Language) -> String {
    let header = command.get_styles().get_header();
    let heading = |text: &str| format!("{header}{text}{header:#}");
    let mut sections = Vec::new();
    if command
        .get_subcommands()
        .any(|subcommand| !subcommand.is_hide_set())
    {
        sections.push(format!(
            "{}\n{{subcommands}}",
            heading(cli::commands_heading(language))
        ));
    }
    if command.get_positionals().any(|arg| !arg.is_hide_set()) {
        sections.push(format!(
            "{}\n{{positionals}}",
            heading(cli::arguments_heading(language))
        ));
    }
    if command
        .get_arguments()
        .any(|arg| !arg.is_positional() && !arg.is_hide_set())
    {
        sections.push(format!(
            "{}\n{{options}}",
            heading(cli::options_heading(language))
        ));
    }
    let mut template = format!(
        "{{before-help}}{{about-with-newline}}\n{} {{usage}}",
        heading(cli::usage_heading(language))
    );
    if !sections.is_empty() {
        template.push_str("\n\n");
        template.push_str(&sections.join("\n\n"));
    }
    template.push_str("{after-help}");
    template
}

/// Checks a tool's localized command against what clap derives for it, for
/// the tools' tests: in English every about and help, and so the short and
/// long help of every (sub)command, is byte for byte the derived one; in
/// Korean every about and help (and long ones where clap has them) is Korean
/// and the headings are the catalog's.
pub fn verify(derived: Command, localized: impl Fn(Language) -> Command) {
    let mut derived = derived;
    derived.build();
    let english = localized(Language::En);
    english.clone().debug_assert();
    same_text(&derived, &english, derived.get_name());
    let (mut want, mut got) = (Vec::new(), Vec::new());
    helps(&mut derived, String::new(), &mut want);
    helps(&mut english.clone(), String::new(), &mut got);
    assert_eq!(
        want.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        got.iter().map(|(path, _)| path).collect::<Vec<_>>()
    );
    for ((path, want), (_, got)) in want.iter().zip(&got) {
        assert_eq!(
            got, want,
            "English help of `{path}` differs from the derived help"
        );
    }

    let korean = localized(Language::Ko);
    korean.clone().debug_assert();
    korean_tree(&korean, korean.get_name());
    let mut rendered = Vec::new();
    helps(&mut korean.clone(), String::new(), &mut rendered);
    for (path, help) in &rendered {
        assert!(
            help.contains(cli::usage_heading(Language::Ko)),
            "`{path}` has no Korean usage heading"
        );
        for english in [
            cli::usage_heading(Language::En),
            cli::commands_heading(Language::En),
            cli::arguments_heading(Language::En),
            cli::options_heading(Language::En),
            cli::possible_values_heading(Language::En),
            "[default: ",
            "[possible values: ",
            "[env: ",
            "[alias",
        ] {
            assert!(!help.contains(english), "`{path}` still says {english}");
        }
    }
}

/// The English catalog says what the derive says, text by text, so a
/// mismatch names the key to fix rather than a whole help page.
fn same_text(derived: &Command, english: &Command, path: &str) {
    let text = |value: Option<&clap::builder::StyledStr>| value.map(ToString::to_string);
    assert_eq!(
        text(english.get_about()),
        text(derived.get_about()),
        "about of `{path}`"
    );
    assert_eq!(
        text(english.get_long_about()),
        text(derived.get_long_about()),
        "long about of `{path}`"
    );
    for want in derived.get_arguments() {
        let id = want.get_id();
        let got = english
            .get_arguments()
            .find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("`{path}` lost {id}"));
        assert_eq!(
            text(got.get_help()),
            text(want.get_help()),
            "help of `{path}` {id}"
        );
        assert_eq!(
            text(got.get_long_help()),
            text(want.get_long_help()),
            "long help of `{path}` {id}"
        );
        let values = |arg: &Arg| {
            arg.get_possible_values()
                .into_iter()
                .map(|value| (value.get_name().to_owned(), text(value.get_help())))
                .collect::<Vec<_>>()
        };
        assert_eq!(values(got), values(want), "values of `{path}` {id}");
    }
    for want in derived.get_subcommands() {
        let name = want.get_name();
        let got = english
            .find_subcommand(name)
            .unwrap_or_else(|| panic!("`{path}` lost {name}"));
        same_text(want, got, &format!("{path} {name}"));
    }
}

fn helps(command: &mut Command, path: String, out: &mut Vec<(String, String)>) {
    command.build();
    out.push((format!("{path} -h"), command.render_help().to_string()));
    out.push((
        format!("{path} --help"),
        command.render_long_help().to_string(),
    ));
    for subcommand in command.get_subcommands_mut() {
        let path = format!("{path} {}", subcommand.get_name())
            .trim()
            .to_owned();
        helps(subcommand, path, out);
    }
}

fn korean_tree(command: &Command, path: &str) {
    let hangul = |text: &str| text.chars().any(|ch| ('가'..='힣').contains(&ch));
    let korean = |what: &str, text: Option<String>, required: bool| {
        assert!(
            match &text {
                Some(text) => hangul(text),
                None => !required,
            },
            "`{path}` has no Korean {what}: {text:?}"
        );
    };
    korean("about", command.get_about().map(ToString::to_string), true);
    korean(
        "long about",
        command.get_long_about().map(ToString::to_string),
        false,
    );
    for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
        let id = arg.get_id();
        korean(
            &format!("help for {id}"),
            arg.get_help().map(ToString::to_string),
            true,
        );
        korean(
            &format!("long help for {id}"),
            arg.get_long_help().map(ToString::to_string),
            false,
        );
        for value in arg.get_possible_values() {
            korean(
                &format!("help for {id} {}", value.get_name()),
                value.get_help().map(ToString::to_string),
                false,
            );
        }
    }
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        korean_tree(subcommand, &format!("{path} {}", subcommand.get_name()));
    }
}
