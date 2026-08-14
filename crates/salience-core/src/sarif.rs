//! SARIF 2.1.0 projection of the sidecar artifact.
//!
//! Code-scanning consumers (GitHub code scanning, SARIF viewers, IDE
//! extensions) ingest SARIF and nothing else, so this is the bridge that lets
//! them display salience without learning the sidecar schema. Three
//! commitments shape the projection:
//!
//! - **It consumes [`Sidecar`], never a frontend's IR.** Anything that can
//!   produce the artifact — single file, JVM class, whole cargo package — gets
//!   SARIF for free, and the projection can never disagree with the artifact
//!   it was derived from.
//! - **One result per line, not per span.** SARIF viewers anchor annotations
//!   on `region.startLine`; a multi-line span emitted as one result would
//!   highlight only its first line and silently drop the rest.
//! - **Only requested tiers become results.** Every line of every function
//!   carries a tier, and a scanner that reports all of them buries the signal
//!   under plumbing. The caller names the tiers worth surfacing; `core` alone
//!   is the intended default.

use std::path::Path;

use serde::Serialize;

use crate::artifact::Sidecar;
use crate::salience::Tier;

/// The `$schema` URI SARIF 2.1.0 logs identify themselves with.
pub const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";

/// The SARIF spec version emitted.
pub const SARIF_VERSION: &str = "2.1.0";

/// A complete SARIF log. One conversion produces one run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SarifLog {
    /// Always [`SARIF_SCHEMA`].
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Always [`SARIF_VERSION`].
    pub version: String,
    /// Exactly one run: the artifact describes one analysis invocation.
    pub runs: Vec<Run>,
}

/// One analysis run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    /// The producing tool.
    pub tool: Tool,
    /// One result per reported line, in artifact order.
    pub results: Vec<SarifResult>,
}

/// The tool component of a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    /// The driver that produced the results.
    pub driver: Driver,
}

/// The tool driver: identity plus the rules its results reference.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Driver {
    /// Tool name, always `salience`.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// Repository URL, from workspace metadata.
    pub information_uri: String,
    /// One reporting descriptor per requested tier.
    pub rules: Vec<Rule>,
}

/// A reporting descriptor: one per tier that can appear in the results.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// `salience/<tier>`, e.g. `salience/core`.
    pub id: String,
    /// What the tier means, in the artifact's own terms.
    pub short_description: Message,
}

/// A SARIF message: plain text only, no markdown variant.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    /// The message text.
    pub text: String,
}

/// One reported line.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifResult {
    /// The tier's rule, e.g. `salience/core`.
    pub rule_id: String,
    /// Always `note`: salience describes code, it does not diagnose defects,
    /// and any level above `note` would fail builds that gate on warnings.
    pub level: String,
    /// Tier and score in prose, e.g. `core line (panel 0.87)`.
    pub message: Message,
    /// Exactly one location: the line itself.
    pub locations: Vec<Location>,
    /// The raw numbers, for consumers that filter or rank rather than read.
    pub properties: ResultProperties,
}

/// A result location.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// The physical (file plus region) location.
    pub physical_location: PhysicalLocation,
}

/// File plus line region.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalLocation {
    /// The file the region refers to.
    pub artifact_location: ArtifactLocation,
    /// The line itself.
    pub region: Region,
}

/// The file a result points into.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArtifactLocation {
    /// The artifact's file path — made relative when it sits under the
    /// analyzed root, forward slashes throughout.
    pub uri: String,
}

/// A single-line region. `startLine` alone means the whole line, which is
/// exactly the artifact's granularity: spans never carry column information.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    /// 1-based source line.
    pub start_line: u32,
}

/// The property bag on every result: the same numbers the sidecar carries,
/// so a SARIF-only consumer can threshold on score without a second parse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResultProperties {
    /// Continuous salience, `0.0..=1.0`, two decimals as in the sidecar.
    pub score: f64,
    /// Tier name, duplicated from the rule id for filter expressions that
    /// only see the bag.
    pub tier: String,
}

impl SarifLog {
    /// Projects `sidecar` into a SARIF log, emitting one result per line of
    /// every span whose tier is in `tiers`.
    ///
    /// `root` is the analyzed root when there is one (cargo crate mode): a
    /// file path under it is reported relative to it, which is what
    /// code-scanning uploads expect. The strip is purely lexical — no
    /// filesystem access — so the projection stays deterministic and paths
    /// that do not sit under the root pass through unchanged.
    #[must_use]
    pub fn from_sidecar(sidecar: &Sidecar, tiers: &[Tier], root: Option<&Path>) -> Self {
        // Fixed descending-salience order: dedups the request and keeps rule
        // order independent of the order the caller listed tiers in.
        let reported: Vec<Tier> = [Tier::Core, Tier::Boundary, Tier::Plumbing, Tier::Inert]
            .into_iter()
            .filter(|t| tiers.contains(t))
            .collect();

        let rules = reported
            .iter()
            .map(|t| Rule {
                id: format!("salience/{}", t.name()),
                short_description: Message {
                    text: tier_description(*t).to_owned(),
                },
            })
            .collect();

        let mut results = Vec::new();
        for f in &sidecar.functions {
            // Per-function file wins when present (crate mode); otherwise the
            // sidecar-level file covers every function, as in the artifact.
            let file = if f.file.is_empty() {
                &sidecar.file
            } else {
                &f.file
            };
            let uri = relative_uri(file, root);
            for s in &f.spans {
                if !reported.iter().any(|t| t.name() == s.tier) {
                    continue;
                }
                for line in s.start..=s.end {
                    results.push(SarifResult {
                        rule_id: format!("salience/{}", s.tier),
                        level: "note".to_owned(),
                        // The panel is the only scorer intended for consumers
                        // (see the CLI's --scorer help), so the message names
                        // it rather than carrying a scorer field the artifact
                        // does not have.
                        message: Message {
                            text: format!("{} line (panel {:.2})", s.tier, s.score),
                        },
                        locations: vec![Location {
                            physical_location: PhysicalLocation {
                                artifact_location: ArtifactLocation { uri: uri.clone() },
                                region: Region { start_line: line },
                            },
                        }],
                        properties: ResultProperties {
                            score: s.score,
                            tier: s.tier.clone(),
                        },
                    });
                }
            }
        }

        Self {
            schema: SARIF_SCHEMA.to_owned(),
            version: SARIF_VERSION.to_owned(),
            runs: vec![Run {
                tool: Tool {
                    driver: Driver {
                        name: "salience".to_owned(),
                        version: env!("CARGO_PKG_VERSION").to_owned(),
                        information_uri: env!("CARGO_PKG_REPOSITORY").to_owned(),
                        rules,
                    },
                },
                results,
            }],
        }
    }
}

/// The tier's meaning, in the same terms the crate docs use.
fn tier_description(tier: Tier) -> &'static str {
    match tier {
        Tier::Core => {
            "Behavior-carrying line: branch predicates, loop-carried dataflow, \
             or a def-use chain reaching an effect"
        }
        Tier::Boundary => {
            "Frontier line: a return, throw, state write, or call into an \
             opaque dependency"
        }
        Tier::Plumbing => "Present but not behavior-carrying: local shuffling, unused results",
        Tier::Inert => "Denylisted call, or computation that exists only to feed one",
    }
}

/// The artifact's file path as a SARIF URI: relative to `root` when it sits
/// under it, forward slashes either way. The backslash replacement is for
/// paths produced on Windows; a URI's segment separator is `/` regardless of
/// the platform the analysis ran on.
///
/// Everything outside the RFC 3986 unreserved set plus `/` is
/// percent-encoded: SARIF requires `uri` to be a valid URI reference, and
/// code-scanning uploads reject logs with raw spaces or non-ASCII bytes in
/// it. Over-encoding the odd pchar (`+`, `,`) keeps the table small and
/// still names the same resource; encoding `:` also stops a Windows drive
/// segment from parsing as a URI scheme.
fn relative_uri(file: &str, root: Option<&Path>) -> String {
    let path = Path::new(file);
    let stripped = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path);
    let slashed = stripped.to_string_lossy().replace('\\', "/");
    let mut uri = String::with_capacity(slashed.len());
    for byte in slashed.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    uri
}
