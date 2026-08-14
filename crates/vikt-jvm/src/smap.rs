//! JSR-45 `SourceDebugExtension` (SMAP) parsing.
//!
//! This exists because of a single measured fact. Compile this Kotlin:
//!
//! ```kotlin
//! inline fun <T> timed(label: String, block: () -> T): T { ... }   // lines 9-12
//! fun usesInline(prices: List<Double>): Double = timed("sum") { ... }  // line 56
//! ```
//!
//! and `usesInline`'s `LineNumberTable` contains lines **82, 83, 84, 85** — in a
//! file that is **80 lines long**. Those numbers are not source lines at all.
//! They are positions in a synthetic composite file that only exists inside the
//! SMAP, and they name the inlined body of `timed`. A `map { }` call in the same
//! class produces lines 86-89, which belong to `kotlin/collections/_Collections.kt`
//! — the standard library, not the user's file at all.
//!
//! A consumer that reads `LineNumberTable` and stops there therefore attributes
//! spans to lines that do not exist, and silently mixes in code from files the
//! developer never opened. For a tool whose whole output is "line N matters",
//! that is not a rough edge; it is wrong output.
//!
//! The fix is the `KotlinDebug` stratum, which is emitted precisely for this and
//! is what the IntelliJ debugger steps through. It maps every inflated output
//! line back to the **call site** in the real file:
//!
//! ```text
//! *S KotlinDebug
//! *F
//! + 1 Orders.kt
//! demo/Processor
//! *L
//! 56#1:82,4      <- output lines 82..85 are all line 56 of file 1
//! 79#1:86        <- output line 86 is line 79
//! 79#1:87,3      <- output lines 87..89 are line 79
//! *E
//! ```
//!
//! Collapsing onto the call site is the right answer for a importance map, not
//! merely the convenient one: the developer sees `timed("sum") { ... }` on line
//! 56, and the work done by the inlined body really is work that line causes.

use std::collections::BTreeMap;

/// One `*L` entry: a run of output lines standing for a run of input lines.
///
/// The JSR-45 grammar is
/// `InputStartLine#LineFileID,RepeatCount:OutputStartLine,OutputLineIncrement`,
/// with `RepeatCount` and `OutputLineIncrement` both defaulting to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineMapping {
    input_start: u32,
    file_id: u32,
    repeat: u32,
    output_start: u32,
    output_increment: u32,
}

impl LineMapping {
    /// The input line an output line stands for, if this mapping covers it.
    fn resolve(&self, output: u32) -> Option<(u32, u32)> {
        let span = self.repeat.max(1) * self.output_increment.max(1);
        if output < self.output_start || output >= self.output_start + span {
            return None;
        }
        let offset = (output - self.output_start) / self.output_increment.max(1);
        Some((self.input_start + offset, self.file_id))
    }
}

/// A parsed `SourceDebugExtension`.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// Mappings from the `KotlinDebug` stratum when present, otherwise from the
    /// default stratum.
    mappings: Vec<LineMapping>,
    /// File id to declared file name, for reporting.
    files: BTreeMap<u32, String>,
    /// Which stratum the mappings came from.
    stratum: String,
}

impl SourceMap {
    /// Parses an SMAP payload. Returns `None` when the text is not an SMAP or
    /// carries no usable line mappings.
    ///
    /// Malformed entries are skipped rather than failing the parse: a partially
    /// readable map is strictly better than none, and the alternative is
    /// discarding correct mappings because one line of a vendor-generated
    /// attribute was unexpected.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()?.trim() != "SMAP" {
            return None;
        }
        let _output_file = lines.next()?;
        let default_stratum = lines.next()?.trim().to_owned();

        // Collect every stratum, then choose. `KotlinDebug` is preferred because
        // it is the one that maps inlined output lines back to their call site
        // in the original file; the default `Kotlin` stratum maps them to the
        // *declaration* site in whatever file that is, which is not a line the
        // reader of this file can act on.
        let mut strata: BTreeMap<String, (Vec<LineMapping>, BTreeMap<u32, String>)> =
            BTreeMap::new();
        let mut current = default_stratum.clone();
        let mut section = Section::None;
        let rest: Vec<&str> = lines.collect();
        let mut i = 0;

        while i < rest.len() {
            let line = rest[i].trim_end();
            i += 1;
            if line == "*E" {
                break;
            }
            if let Some(name) = line.strip_prefix("*S ") {
                name.trim().clone_into(&mut current);
                section = Section::None;
                continue;
            }
            match line {
                "*F" => {
                    section = Section::Files;
                    continue;
                }
                "*L" => {
                    section = Section::Lines;
                    continue;
                }
                // `*O`/`*C` open and close an embedded stratum, and `*V` is a
                // vendor section. None affect line resolution.
                _ if line.starts_with('*') => {
                    section = Section::None;
                    continue;
                }
                _ => {}
            }

            let entry = strata.entry(current.clone()).or_default();
            match section {
                Section::Files => {
                    // Either `+ <id> <name>` followed by a path line, or
                    // `<id> <name>` on its own.
                    let (spec, has_path) = match line.strip_prefix("+ ") {
                        Some(s) => (s, true),
                        None => (line, false),
                    };
                    let mut parts = spec.trim().splitn(2, ' ');
                    if let (Some(id), Some(name)) = (parts.next(), parts.next())
                        && let Ok(id) = id.trim().parse::<u32>()
                    {
                        entry.1.insert(id, name.trim().to_owned());
                    }
                    if has_path {
                        i += 1; // consume the path line
                    }
                }
                Section::Lines => {
                    if let Some(m) = parse_line_mapping(line.trim()) {
                        entry.0.push(m);
                    }
                }
                Section::None => {}
            }
        }

        // Prefer KotlinDebug, then the declared default, then anything with
        // mappings: in a fixed order so the result is reproducible.
        let chosen = ["KotlinDebug", default_stratum.as_str()]
            .into_iter()
            .find(|s| strata.get(*s).is_some_and(|e| !e.0.is_empty()))
            .map(str::to_owned)
            .or_else(|| {
                strata
                    .iter()
                    .find(|(_, e)| !e.0.is_empty())
                    .map(|(k, _)| k.clone())
            })?;

        let (mappings, files) = strata.remove(&chosen)?;
        if mappings.is_empty() {
            return None;
        }
        Some(Self {
            mappings,
            files,
            stratum: chosen,
        })
    }

    /// Which stratum the mappings came from.
    #[must_use]
    pub fn stratum(&self) -> &str {
        &self.stratum
    }

    /// Resolves a `LineNumberTable` entry to a real line in the class's own
    /// source file.
    ///
    /// - `Resolved(line)` — the output line stands for `line` in this file.
    ///   Identity for ordinary code; the call-site line for an inlined body.
    /// - `Foreign { file }` — it belongs to a different source file. Callers
    ///   should drop it rather than claim a line in this one.
    #[must_use]
    pub fn resolve(&self, output_line: u32) -> Resolution {
        for m in &self.mappings {
            if let Some((input, file_id)) = m.resolve(output_line) {
                // File 1 is the class's own source by JSR-45 convention, and
                // the KotlinDebug stratum declares only that file.
                if file_id == 1 {
                    return Resolution::Resolved(input);
                }
                return Resolution::Foreign {
                    file: self
                        .files
                        .get(&file_id)
                        .cloned()
                        .unwrap_or_else(|| format!("file #{file_id}")),
                };
            }
        }
        // Not covered by any mapping: an ordinary line, which the strata leave
        // implicit because it maps to itself.
        Resolution::Resolved(output_line)
    }

    /// The highest output line any mapping covers.
    ///
    /// Useful as a sanity check: any `LineNumberTable` entry above this and
    /// above the file's real length is unattributable.
    #[must_use]
    pub fn max_output_line(&self) -> u32 {
        self.mappings
            .iter()
            .map(|m| m.output_start + m.repeat.max(1) * m.output_increment.max(1))
            .max()
            .unwrap_or(0)
    }
}

/// What an output line turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A real line in the class's own source file.
    Resolved(u32),
    /// A line belonging to some other file, inlined into this class.
    Foreign {
        /// The declared name of that file.
        file: String,
    },
}

/// Which `*` section the parser is inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Files,
    Lines,
}

/// Parses `InputStartLine#LineFileID,RepeatCount:OutputStartLine,OutputLineIncrement`.
///
/// Every part except `InputStartLine` and `OutputStartLine` is optional.
fn parse_line_mapping(s: &str) -> Option<LineMapping> {
    let (lhs, rhs) = s.split_once(':')?;

    // Left: InputStartLine [ # LineFileID ] [ , RepeatCount ]
    let (input_part, repeat) = match lhs.split_once(',') {
        Some((a, b)) => (a, b.trim().parse().ok()?),
        None => (lhs, 1),
    };
    let (input_start, file_id) = match input_part.split_once('#') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => (input_part.trim().parse().ok()?, 1),
    };

    // Right: OutputStartLine [ , OutputLineIncrement ]
    let (output_start, output_increment) = match rhs.split_once(',') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => (rhs.trim().parse().ok()?, 1),
    };

    Some(LineMapping {
        input_start,
        file_id,
        repeat,
        output_start,
        output_increment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact attribute `kotlinc 2.1.20` emitted for the demo fixture. Every
    /// number here was read out of a real class file.
    const REAL: &str = "SMAP\n\
Orders.kt\n\
Kotlin\n\
*S Kotlin\n\
*F\n\
+ 1 Orders.kt\n\
demo/Processor\n\
+ 2 Orders.kt\n\
demo/OrdersKt\n\
+ 3 _Collections.kt\n\
kotlin/collections/CollectionsKt___CollectionsKt\n\
*L\n\
1#1,81:1\n\
9#2,4:82\n\
1563#3:86\n\
1634#3,3:87\n\
*S KotlinDebug\n\
*F\n\
+ 1 Orders.kt\n\
demo/Processor\n\
*L\n\
56#1:82,4\n\
79#1:86\n\
79#1:87,3\n\
*E\n";

    #[test]
    fn prefers_the_kotlin_debug_stratum() {
        let map = SourceMap::parse(REAL).expect("parses");
        assert_eq!(map.stratum(), "KotlinDebug");
    }

    /// The headline fix: the four impossible lines an 80-line file reported
    /// all collapse onto the call site.
    #[test]
    fn inlined_body_collapses_onto_the_call_site() {
        let map = SourceMap::parse(REAL).expect("parses");
        for output in 82..=85 {
            assert_eq!(
                map.resolve(output),
                Resolution::Resolved(56),
                "output line {output} should resolve to the call site on line 56"
            );
        }
    }

    /// A second inline call site, and one whose inlined code comes from the
    /// standard library rather than this file.
    #[test]
    fn stdlib_inlining_also_collapses_onto_its_call_site() {
        let map = SourceMap::parse(REAL).expect("parses");
        for output in 86..=89 {
            assert_eq!(map.resolve(output), Resolution::Resolved(79));
        }
    }

    /// Ordinary lines are untouched.
    #[test]
    fn ordinary_lines_map_to_themselves() {
        let map = SourceMap::parse(REAL).expect("parses");
        for output in [1, 19, 24, 56, 79, 81] {
            assert_eq!(map.resolve(output), Resolution::Resolved(output));
        }
    }

    /// Without the `KotlinDebug` stratum, output 82 belongs to file 2 and must
    /// be reported as foreign rather than claimed as a line of this file.
    #[test]
    fn foreign_files_are_reported_when_no_call_site_mapping_exists() {
        let only_kotlin = REAL.split("*S KotlinDebug").next().unwrap().to_owned() + "*E\n";
        let map = SourceMap::parse(&only_kotlin).expect("parses");
        assert_eq!(map.stratum(), "Kotlin");
        match map.resolve(86) {
            Resolution::Foreign { file } => assert_eq!(file, "_Collections.kt"),
            other @ Resolution::Resolved(_) => panic!("expected a foreign file, got {other:?}"),
        }
        // File 1's own range is still identity.
        assert_eq!(map.resolve(40), Resolution::Resolved(40));
    }

    #[test]
    fn mapping_grammar_defaults() {
        // No repeat, no increment.
        assert_eq!(
            parse_line_mapping("79#1:86"),
            Some(LineMapping {
                input_start: 79,
                file_id: 1,
                repeat: 1,
                output_start: 86,
                output_increment: 1
            })
        );
        // No file id at all.
        assert_eq!(
            parse_line_mapping("5,3:10,2"),
            Some(LineMapping {
                input_start: 5,
                file_id: 1,
                repeat: 3,
                output_start: 10,
                output_increment: 2
            })
        );
        assert_eq!(parse_line_mapping("nonsense"), None);
    }

    #[test]
    fn rejects_non_smap_text() {
        assert!(SourceMap::parse("").is_none());
        assert!(SourceMap::parse("not an smap").is_none());
        // Well-formed header but no mappings is not usable.
        assert!(SourceMap::parse("SMAP\nA.kt\nKotlin\n*E\n").is_none());
    }
}
