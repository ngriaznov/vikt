//! Per-language node-kind/field-name vocabularies.
//!
//! [`GrammarTable`] is the only thing that varies between languages; the
//! walker in [`crate::walk`] is written once against it. A table entry names
//! a tree-sitter node kind and, where the grammar fields it, the field names
//! the walker reads off that kind — see the module docs on `walk` for how
//! each slot is used.
//!
//! Every slot that takes a *field* name also tolerates the field going
//! unused (`""`): Rust, Python and Java field essentially everything the
//! walker needs, but `tree-sitter-kotlin-ng`'s grammar leaves `call_expression`,
//! `navigation_expression`, `for_statement`, `property_declaration`,
//! `when_expression`, `try_expression` and `catch_block` completely
//! unfielded (verified against the crate's own `node-types.json`, not
//! guessed) — the walker falls back to a node-kind search or a positional
//! pick from named children for those slots, see `walk`'s `field_or_*`
//! helpers.

/// One language's node-kind/field-name vocabulary for the generic walker.
pub(crate) struct GrammarTable {
    /// Generator string recorded in a lowered function's provenance.
    pub generator: &'static str,

    /// Whether every `block_kinds` node introduces a fresh scope (Rust/Java/
    /// Kotlin: `let`/local declarations shadow per block) or the function
    /// has one flat scope throughout (Python: assignment binds for the
    /// whole function, `if`/`while`/`for` bodies do not nest scope).
    pub block_scoped: bool,

    pub function_kinds: &'static [&'static str],
    pub function_name_field: &'static str,
    pub function_params_field: &'static str,
    /// Kind-search fallback for `function_params_field` when the language
    /// doesn't field it at all (Kotlin: `function_declaration` has no
    /// `parameters` field, just an unfielded `function_value_parameters`
    /// child).
    pub function_params_kind: &'static str,
    pub function_body_field: &'static str,
    /// Kind-search fallback for `function_body_field` (Kotlin's
    /// `function_body`, likewise unfielded on its parent).
    pub function_body_kind: &'static str,

    /// A class-like declaration whose methods get an owning-type prefix
    /// (`Type::method`, matching the JVM frontend's naming) and whose
    /// direct field/property declarations belong to the `<module>`
    /// wrapper. Empty for Rust/Python, which have no class body distinct
    /// from a block.
    pub class_kinds: &'static [&'static str],
    pub class_name_field: &'static str,
    pub class_body_field: &'static str,
    /// Kind-search fallback for `class_body_field` (Kotlin's
    /// `class_declaration` doesn't field its `class_body` child).
    pub class_body_kind: &'static str,

    /// A function-like node's own receiver-parameter field, naming its
    /// owning type inline rather than through a `class_kinds` ancestor - Go
    /// alone has this: `method_declaration.receiver`, a one-parameter
    /// `parameter_list` (methods live as top-level siblings, not nested
    /// inside any type body). Combined with `receiver_type_field`/
    /// `receiver_pointer_kind` in `walk::receiver_owner` to still produce
    /// `Type::method` naming - see that function's docs. Empty for every
    /// language reached instead through `class_kinds` (Java, Kotlin) or
    /// with no method-receiver concept at all (Rust, Python).
    pub receiver_field: &'static str,
    /// The receiver parameter's own type field (Go: `type`, on the
    /// `parameter_declaration` inside `receiver_field`). Unused when
    /// `receiver_field` is empty.
    pub receiver_type_field: &'static str,
    /// A pointer-wrapper kind around the receiver's type (Go: `pointer_type`
    /// wraps a `*T` receiver's `type_identifier`) - unwrapped one level so
    /// `(c *Counter)` and `(c Counter)` both name the same owner `Counter`.
    /// Unused when `receiver_field` is empty.
    pub receiver_pointer_kind: &'static str,

    /// A node whose named children are a sequence of statements.
    pub block_kinds: &'static [&'static str],
    /// Nodes that carry no meaning of their own and transparently unwrap to
    /// their `body` field, or their last named child when there is no such
    /// field (e.g. Rust's `expression_statement`, which wraps exactly the
    /// expression it terminates; Kotlin's `function_body`, which wraps
    /// either a `block` or a tail expression for `= expr` bodies; Go's
    /// `block`, which wraps a `statement_list` holding the real statements,
    /// or nothing at all when empty - `statement_list` itself is also a
    /// `block_kinds` entry for Go, so an empty `block` (no `statement_list`
    /// child to unwrap to) still dispatches correctly as an empty sequence
    /// rather than falling through to an expression-body reading).
    pub wrapper_kinds: &'static [&'static str],
    /// A node whose named children are spliced directly into the
    /// surrounding statement sequence in place of the node itself, in the
    /// *caller's* scope rather than a fresh one - Go's `var_declaration`/
    /// `const_declaration`, each of whose named children is one `var_spec`/
    /// `const_spec` (the parenthesized block form holds more than one; the
    /// bare form holds exactly one). Unlike `wrapper_kinds`, which unwraps
    /// to a single inner node, every named child here becomes its own
    /// dispatchable statement - and unlike `block_kinds`, no new scope is
    /// pushed, since a `var`/`const` block's declared names must stay
    /// visible to the statements after it, not vanish when a transient
    /// block scope pops. Empty for every other language.
    pub flatten_kinds: &'static [&'static str],

    /// A fresh-binding declaration form, e.g. Rust's `let`, Java's
    /// `local_variable_declaration`/`field_declaration`, Kotlin's
    /// `property_declaration`. Empty for a language where assignment is the
    /// only binding form (Python).
    pub binding_kinds: &'static [&'static str],
    pub binding_pattern_field: &'static str,
    /// Kind-search fallback for `binding_pattern_field` (Kotlin's
    /// `property_declaration` is entirely unfielded, and a leading
    /// `modifiers` child means "first named child" isn't safe either).
    pub binding_pattern_kind: &'static str,
    pub binding_value_field: &'static str,
    /// A field on a `binding_kinds` node holding a diverging else-block for
    /// a pattern-match binding that can fail, e.g. Rust's `let`-else
    /// `alternative` (a `block` that must return/break/continue/panic - the
    /// language forbids falling off its end). Empty for every other
    /// language modeled here; none has this construct.
    pub binding_alt_field: &'static str,
    /// A binding node whose actual name/value live one level down, in a
    /// list of declarator children (Java: `int a = 1, b = 2;` is one
    /// `local_variable_declaration` with two `variable_declarator`s under
    /// its `declarator` field). When set, `binding_pattern_field`/
    /// `binding_value_field` are ignored and the walker lowers one
    /// statement per declarator instead — mirrors `vikt-js`'s "one node per
    /// declarator for multi-decl `let`/`const`".
    pub binding_declarator_field: &'static str,
    pub binding_declarator_name_field: &'static str,
    pub binding_declarator_value_field: &'static str,
    /// A second fresh-binding shape needing its own pattern/value field
    /// names, distinct from `binding_pattern_field`/`binding_value_field` -
    /// Go alone needs two at once: `short_var_declaration`/
    /// `receive_statement`'s `left`/`right` (a whole pattern-list field,
    /// exactly the Rust/Python shape) vs. `var_spec`/`const_spec`'s `name`/
    /// `value` (a bare identifier field, only the *first* name captured
    /// when a spec declares more than one - `var a, b = 1, 2` binds `a`
    /// only; `b` resolves as an ambient read at its first use, a documented
    /// v1 gap since the grammar repeats the `name` field rather than
    /// wrapping every name in one list node the way `left`/`right` do).
    /// Every `binding_kinds` member listed here resolves through
    /// `binding_pattern_field2`/`binding_value_field2` instead of the
    /// primary pair; every other member keeps using the primary pair
    /// unaffected. Empty for every language but Go.
    pub binding_kinds2: &'static [&'static str],
    pub binding_pattern_field2: &'static str,
    pub binding_value_field2: &'static str,

    pub assign_kinds: &'static [&'static str],
    /// A dedicated compound-assignment node kind, e.g. Rust's
    /// `compound_assignment_expr`, Python's `augmented_assignment`. Empty
    /// for a language that reuses `assign_kinds` for both and disambiguates
    /// via `assign_operator_field`'s text instead (Java, Kotlin, Go: one
    /// `assignment_expression`/`assignment` kind, `+=` vs `=` is just the
    /// operator field's text).
    pub compound_assign_kinds: &'static [&'static str],
    pub assign_operator_field: &'static str,
    /// Whether a plain-variable assignment target declares (Python: the only
    /// binding form, reusing the slot on re-assignment) or must resolve an
    /// existing binding (Rust/Java/Kotlin/Go: `x = 1` is only legal after
    /// some prior declaration — Go's declaring form is `:=`, modeled
    /// separately via `binding_kinds2`, above).
    pub assign_declares: bool,
    pub assign_left_field: &'static str,
    pub assign_right_field: &'static str,

    pub call_kinds: &'static [&'static str],
    /// A single field holding the callee position, itself either an
    /// identifier or a `member_access_kinds` chain (Rust/Python's
    /// `function`). Empty when the language either splits object/name into
    /// two fields (`call_object_field`/`call_name_field`, Java) or fields
    /// neither at all (Kotlin: callee is simply the first named child).
    pub call_callee_field: &'static str,
    /// Paired fields for a call node that names its receiver and method
    /// separately rather than nesting one under the other (Java's
    /// `method_invocation`: `object` is optional, `name` is the bare method
    /// identifier — there is no single node that is "the callee").
    pub call_object_field: &'static str,
    pub call_name_field: &'static str,

    /// Method/attribute access, e.g. Rust's `field_expression`, Python's
    /// `attribute`, Kotlin's `navigation_expression`. Used both to classify
    /// an assignment target as a state write and to extract a dotted callee
    /// name for method calls.
    pub member_access_kinds: &'static [&'static str],
    pub member_object_field: &'static str,
    pub member_property_field: &'static str,

    pub if_kinds: &'static [&'static str],
    pub if_cond_field: &'static str,
    /// Positional fallback (named-child index) when `if_then_field`/
    /// `if_cond_field` is empty (Kotlin fields only `condition`; then/else
    /// are just the next named children in order).
    pub if_then_field: &'static str,
    /// Field carrying the `else`/`elif` continuation(s). May be multi-valued
    /// (Python: zero or more `elif_clause` followed by an optional
    /// `else_clause`), so the walker reads it with `children_by_field_name`.
    /// Empty for a language that fields neither (Kotlin: read positionally,
    /// single-valued).
    pub if_alt_field: &'static str,
    /// A distinct "else if" node kind with its own condition/then fields
    /// (Python's `elif_clause`), reusing `if_cond_field`/`if_then_field`.
    /// `None` when the language chains via nested `if` inside its else
    /// position instead (Rust, Java, Kotlin).
    pub elif_kind: Option<&'static str>,

    pub while_kinds: &'static [&'static str],
    pub while_cond_field: &'static str,
    pub while_body_field: &'static str,

    /// An unconditional loop with no condition to evaluate at all, e.g.
    /// Rust's `loop { .. }` (as opposed to `while true { .. }`, which is a
    /// real `while_kinds` node with a literal condition). Empty for every
    /// other language modeled here - Python, Java, Kotlin and Go have no
    /// dedicated condition-less loop node (Go's bare `for {}` is instead
    /// recognized structurally by `for_range_kind`'s shape probe, below).
    pub loop_kinds: &'static [&'static str],
    pub loop_body_field: &'static str,

    /// A for-each loop: pattern/binding, iterated value, body. None of the
    /// five languages' tables here model a C-style three-clause `for` (Java's
    /// plain `for_statement` falls back to a generic unit — a documented v1
    /// simplification, since `enhanced_for_statement` is the idiomatic form;
    /// Go's `for_clause` shape gets the same treatment, below).
    pub for_kinds: &'static [&'static str],
    pub for_pattern_field: &'static str,
    pub for_value_field: &'static str,
    pub for_body_field: &'static str,
    /// Go's `for_statement` is one grammar kind covering four shapes: no
    /// clause at all (an unconditional loop, `loop_kinds`' shape), a bare
    /// boolean expression (`while_kinds`' shape), a `for_clause` (three-part
    /// C-style, deliberately unmodeled - the same v1 gap as Java's plain
    /// `for_statement`, falling through to the generic construct fallback),
    /// or a `range_clause` (for-each, this field's own shape - `left`/
    /// `right` on the clause itself, not on `for_statement`, are what
    /// `for_pattern_field`/`for_value_field` above resolve against for Go).
    /// Non-empty here switches `lower_for` onto `FnCtx::lower_go_for`, a
    /// shape probe that dispatches to whichever of the other three
    /// treatments actually fits, so all three still get the identical
    /// loop-with-back-edge modeling their own dedicated node kinds get
    /// elsewhere. Empty (every other table) keeps the ordinary single-shape
    /// `for_kinds` path unaffected.
    pub for_range_kind: &'static str,
    /// The C-style clause's own kind, checked only by the same probe -
    /// tried first, so its presence is never mistaken for a bare condition.
    /// Empty everywhere else.
    pub for_clause_kind: &'static str,

    pub return_kinds: &'static [&'static str],
    pub throw_kinds: &'static [&'static str],
    pub break_kinds: &'static [&'static str],
    pub continue_kinds: &'static [&'static str],
    /// `break`/`continue` spelled as a plain `identifier` leaf rather than a
    /// dedicated node kind — `tree-sitter-kotlin-ng` 1.1.0 has no jump-statement
    /// node for the unlabelled form at all (confirmed by parsing one: bare
    /// `continue` lexes as `identifier` text `"continue"`). Checked only
    /// when the node's kind is in `identifier_kinds` and matches exactly, so
    /// it can never misfire on Rust/Python/Java, which all have real
    /// `break_kinds`/`continue_kinds` entries and leave these empty.
    pub break_texts: &'static [&'static str],
    pub continue_texts: &'static [&'static str],

    /// Leaf kind(s) that name a variable. Deliberately excludes a
    /// language's "this is a member/attribute name" leaf kind when that
    /// differs (Rust's `field_identifier`); where the grammar reuses the
    /// plain identifier kind for both (Python's `attribute`, Kotlin's
    /// `navigation_expression`), the walker skips the
    /// `member_property_field` position explicitly instead.
    pub identifier_kinds: &'static [&'static str],

    // ---------------------------------------------------------- match ----
    /// `match`/`switch`/`when` at statement position: the discriminant
    /// becomes a real `Branch` with one successor per arm - see `walk`'s
    /// `lower_match`.
    pub match_kinds: &'static [&'static str],
    pub match_subject_field: &'static str,
    /// Kind-search fallback for `match_subject_field` (Kotlin's subject sits
    /// in an unfielded `when_subject` wrapper, itself found by kind).
    pub match_subject_kind: &'static str,
    /// Field naming the arm list's container. Empty for a language whose
    /// arms are direct children of the match node itself (Kotlin's
    /// `when_expression` has no separate body wrapper).
    pub match_body_field: &'static str,
    pub match_body_kind: &'static str,
    pub match_arm_kinds: &'static [&'static str],
    /// Field naming one arm's pattern/label/condition. Empty when the
    /// language doesn't field it at all (Python/Java: kind-searched instead
    /// via `match_arm_pattern_kind`).
    pub match_arm_pattern_field: &'static str,
    pub match_arm_pattern_kind: &'static str,
    /// Whether an arm can carry more than one pattern value as *separate*
    /// fielded occurrences rather than one node covering all of them
    /// (Kotlin's `when_entry`: `condition: 1, condition: 2 ->`, read with
    /// `children_by_field_name`). `false` for a language where a
    /// multi-value arm is one node either way (Rust's `1 | 2`, Java's
    /// `case 1, 2:` both parse as a single pattern/label node).
    pub match_arm_pattern_multi: bool,
    /// Whether the pattern position can introduce new bindings (Rust/Python
    /// structural pattern matching: `Some(x)` binds `x`) as opposed to being
    /// a plain read of an existing value (Java's `case` labels, Kotlin's
    /// `when` conditions are ordinary expressions, never destructuring).
    pub match_arm_pattern_declares: bool,
    /// A guard field directly on the arm node, sibling to the pattern
    /// (Python's `case_clause.guard`). Empty when the language has no guard
    /// clause at all, or nests it inside the pattern instead (see next).
    pub match_arm_guard_field: &'static str,
    /// A guard field nested one level down, on the pattern/label node
    /// itself rather than the arm (Rust's `match_pattern.condition`).
    pub match_pattern_guard_field: &'static str,

    // -------------------------------------------------------------- try ----
    /// `try`/`except`/`catch`/`finally`. Empty `try_kinds` for a language
    /// with no such construct (Rust: fallible calls use `Result`, not
    /// exceptions) - every other field in this group is then unused.
    pub try_kinds: &'static [&'static str],
    pub try_body_field: &'static str,
    pub try_body_kind: &'static str,
    /// One handler clause, e.g. Python's `except_clause`, Java's
    /// `catch_clause`, Kotlin's `catch_block` - gathered as however many of
    /// these appear as direct children of the `try_kinds` node.
    pub catch_kinds: &'static [&'static str],
    pub catch_body_field: &'static str,
    pub catch_body_kind: &'static str,
    /// Kind-search for a handler's bound exception name when it sits in its
    /// own node (Java's `catch_formal_parameter`, further fielded by
    /// `catch_param_name_field`; Kotlin's bare `identifier`, no further
    /// field needed). Empty for a language whose binding is reached through
    /// `catch_param_as_field` instead (Python).
    pub catch_param_kind: &'static str,
    pub catch_param_name_field: &'static str,
    /// Python-only path to a handler's `as`-bound name: the field holding
    /// the exception value (`except E as e:`'s `value`), checked for the
    /// `as_pattern` kind, then descended through `catch_param_alias_field`.
    /// Empty for every language reached instead through `catch_param_kind`.
    pub catch_param_as_field: &'static str,
    pub catch_param_as_pattern_kind: &'static str,
    pub catch_param_alias_field: &'static str,
    /// Python's `try`/`else`: runs only when the `try` body raised nothing,
    /// so it is lowered as sequential code after the try body's exits, same
    /// treatment as `finally`. Empty for every other language (no such
    /// clause).
    pub try_else_kind: &'static str,
    pub try_else_body_field: &'static str,
    pub finally_kind: &'static str,
    pub finally_body_field: &'static str,
    pub finally_body_kind: &'static str,

    // --------------------------------------------------------- closures ----
    /// A closure/lambda expression, e.g. Rust's `closure_expression`,
    /// Python's `lambda`, Java's `lambda_expression`, Kotlin's
    /// `lambda_literal`. Each becomes its own [`FunctionIr`], named via
    /// `closure_name_format` rather than a source-given name, alongside
    /// `function_kinds` - see `walk`'s closure-naming docs.
    pub closure_kinds: &'static [&'static str],
    pub closure_params_field: &'static str,
    pub closure_params_kind: &'static str,
    pub closure_body_field: &'static str,
    pub closure_body_kind: &'static str,
    /// Whether a closure's statements are direct children of the closure
    /// node itself, with no body wrapper at all to find by field or kind
    /// (Kotlin's `lambda_literal`: `{ x -> stmt; stmt }`, no `block` node
    /// inside). When true, `closure_body_field`/`_kind` are unused; the
    /// body is every named child except the parameter list.
    pub closure_body_is_bare_children: bool,
    /// Template for a closure's synthetic name: `{owner}` becomes the
    /// nearest enclosing function's already-assigned name (or `<module>`,
    /// or the enclosing type for a field-initializer closure), `{idx}` a
    /// 0-based per-owner counter in appearance order. Matches each
    /// frontend's real synthetic convention closely enough for
    /// `Language::is_synthetic`'s brace/angle check to recognize it - see
    /// `vikt-cli::language` - and for calibrate's positional sampler to
    /// skip it as it would a `<module>` wrapper.
    pub closure_name_format: &'static str,
}

pub(crate) static RUST: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-rust",
    block_scoped: true,

    function_kinds: &["function_item"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_params_kind: "",
    function_body_field: "body",
    function_body_kind: "",

    class_kinds: &[],
    class_name_field: "",
    class_body_field: "",
    class_body_kind: "",
    receiver_field: "",
    receiver_type_field: "",
    receiver_pointer_kind: "",

    block_kinds: &["block"],
    wrapper_kinds: &["expression_statement", "unsafe_block", "else_clause"],
    flatten_kinds: &[],

    binding_kinds: &["let_declaration"],
    binding_pattern_field: "pattern",
    binding_pattern_kind: "",
    binding_value_field: "value",
    binding_alt_field: "alternative",
    binding_declarator_field: "",
    binding_declarator_name_field: "",
    binding_declarator_value_field: "",
    binding_kinds2: &[],
    binding_pattern_field2: "",
    binding_value_field2: "",

    assign_kinds: &["assignment_expression"],
    compound_assign_kinds: &["compound_assignment_expr"],
    assign_operator_field: "",
    assign_declares: false,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call_expression"],
    call_callee_field: "function",
    call_object_field: "",
    call_name_field: "",

    member_access_kinds: &["field_expression"],
    member_object_field: "value",
    member_property_field: "field",

    if_kinds: &["if_expression"],
    if_cond_field: "condition",
    if_then_field: "consequence",
    if_alt_field: "alternative",
    elif_kind: None,

    while_kinds: &["while_expression"],
    while_cond_field: "condition",
    while_body_field: "body",

    loop_kinds: &["loop_expression"],
    loop_body_field: "body",

    for_kinds: &["for_expression"],
    for_pattern_field: "pattern",
    for_value_field: "value",
    for_body_field: "body",
    for_range_kind: "",
    for_clause_kind: "",

    return_kinds: &["return_expression"],
    throw_kinds: &[],
    break_kinds: &["break_expression"],
    continue_kinds: &["continue_expression"],
    break_texts: &[],
    continue_texts: &[],

    identifier_kinds: &["identifier"],

    match_kinds: &["match_expression"],
    match_subject_field: "value",
    match_subject_kind: "",
    match_body_field: "body",
    match_body_kind: "",
    match_arm_kinds: &["match_arm"],
    match_arm_pattern_field: "pattern",
    match_arm_pattern_kind: "",
    match_arm_pattern_multi: false,
    match_arm_pattern_declares: true,
    match_arm_guard_field: "",
    // The guard lives inside `match_pattern` itself (`_ if cond => ..`), not
    // as a sibling field on `match_arm`.
    match_pattern_guard_field: "condition",

    try_kinds: &[],
    try_body_field: "",
    try_body_kind: "",
    catch_kinds: &[],
    catch_body_field: "",
    catch_body_kind: "",
    catch_param_kind: "",
    catch_param_name_field: "",
    catch_param_as_field: "",
    catch_param_as_pattern_kind: "",
    catch_param_alias_field: "",
    try_else_kind: "",
    try_else_body_field: "",
    finally_kind: "",
    finally_body_field: "",
    finally_body_kind: "",

    closure_kinds: &["closure_expression"],
    closure_params_field: "parameters",
    closure_params_kind: "",
    closure_body_field: "body",
    closure_body_kind: "",
    closure_body_is_bare_children: false,
    closure_name_format: "{owner}::{closure#{idx}}",
};

pub(crate) static PYTHON: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-python",
    block_scoped: false,

    function_kinds: &["function_definition"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_params_kind: "",
    function_body_field: "body",
    function_body_kind: "",

    class_kinds: &[],
    class_name_field: "",
    class_body_field: "",
    class_body_kind: "",
    receiver_field: "",
    receiver_type_field: "",
    receiver_pointer_kind: "",

    block_kinds: &["block"],
    wrapper_kinds: &["expression_statement", "else_clause"],
    flatten_kinds: &[],

    binding_kinds: &[],
    binding_pattern_field: "",
    binding_pattern_kind: "",
    binding_value_field: "",
    binding_alt_field: "",
    binding_declarator_field: "",
    binding_declarator_name_field: "",
    binding_declarator_value_field: "",
    binding_kinds2: &[],
    binding_pattern_field2: "",
    binding_value_field2: "",

    assign_kinds: &["assignment"],
    compound_assign_kinds: &["augmented_assignment"],
    assign_operator_field: "",
    assign_declares: true,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call"],
    call_callee_field: "function",
    call_object_field: "",
    call_name_field: "",

    member_access_kinds: &["attribute"],
    member_object_field: "object",
    member_property_field: "attribute",

    if_kinds: &["if_statement"],
    if_cond_field: "condition",
    if_then_field: "consequence",
    if_alt_field: "alternative",
    elif_kind: Some("elif_clause"),

    while_kinds: &["while_statement"],
    while_cond_field: "condition",
    while_body_field: "body",

    loop_kinds: &[],
    loop_body_field: "",

    for_kinds: &["for_statement"],
    for_pattern_field: "left",
    for_value_field: "right",
    for_body_field: "body",
    for_range_kind: "",
    for_clause_kind: "",

    return_kinds: &["return_statement"],
    throw_kinds: &["raise_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    break_texts: &[],
    continue_texts: &[],

    identifier_kinds: &["identifier"],

    match_kinds: &["match_statement"],
    match_subject_field: "subject",
    match_subject_kind: "",
    match_body_field: "body",
    match_body_kind: "",
    match_arm_kinds: &["case_clause"],
    match_arm_pattern_field: "",
    match_arm_pattern_kind: "case_pattern",
    match_arm_pattern_multi: false,
    match_arm_pattern_declares: true,
    // A sibling field on `case_clause` (`case [a, b] if a > b:`).
    match_arm_guard_field: "guard",
    match_pattern_guard_field: "",

    try_kinds: &["try_statement"],
    try_body_field: "body",
    try_body_kind: "",
    catch_kinds: &["except_clause"],
    catch_body_field: "",
    catch_body_kind: "block",
    catch_param_kind: "",
    catch_param_name_field: "",
    // `except E as e:` parses as `value: as_pattern { E as alias: e }`;
    // bare `except E:` or `except (A, B):` leaves `value` a plain
    // expression/tuple, no binding to declare.
    catch_param_as_field: "value",
    catch_param_as_pattern_kind: "as_pattern",
    catch_param_alias_field: "alias",
    try_else_kind: "else_clause",
    try_else_body_field: "body",
    finally_kind: "finally_clause",
    finally_body_field: "",
    finally_body_kind: "block",

    closure_kinds: &["lambda"],
    closure_params_field: "parameters",
    closure_params_kind: "",
    closure_body_field: "body",
    closure_body_kind: "",
    closure_body_is_bare_children: false,
    // Real CPython gives every lambda in one scope the same `co_qualname`
    // - no numbering, unlike Rust's MIR closures.
    closure_name_format: "{owner}.<locals>.<lambda>",
};

pub(crate) static JAVA: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-java",
    block_scoped: true,

    function_kinds: &["method_declaration"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_params_kind: "",
    function_body_field: "body",
    function_body_kind: "",

    class_kinds: &["class_declaration"],
    class_name_field: "name",
    class_body_field: "body",
    class_body_kind: "",
    receiver_field: "",
    receiver_type_field: "",
    receiver_pointer_kind: "",

    block_kinds: &["block"],
    wrapper_kinds: &["expression_statement"],
    flatten_kinds: &[],

    binding_kinds: &["local_variable_declaration", "field_declaration"],
    binding_pattern_field: "",
    binding_pattern_kind: "",
    binding_value_field: "",
    binding_alt_field: "",
    binding_declarator_field: "declarator",
    binding_declarator_name_field: "name",
    binding_declarator_value_field: "value",
    binding_kinds2: &[],
    binding_pattern_field2: "",
    binding_value_field2: "",

    assign_kinds: &["assignment_expression"],
    compound_assign_kinds: &[],
    assign_operator_field: "operator",
    assign_declares: false,
    assign_left_field: "left",
    assign_right_field: "right",

    // `object_creation_expression` (`new Foo(...)`) is deliberately not a
    // call kind here: it fields `type`/`arguments`, not a callee position at
    // all, and this frontend's denylist model has nothing to say about
    // same-file constructor calls that bytecode/AST tools couldn't already
    // tell it. A documented v1 gap, not a crash risk — its identifiers still
    // show up as ordinary uses of whatever statement contains it.
    call_kinds: &["method_invocation"],
    call_callee_field: "",
    call_object_field: "object",
    call_name_field: "name",

    member_access_kinds: &["field_access"],
    member_object_field: "object",
    member_property_field: "field",

    if_kinds: &["if_statement"],
    if_cond_field: "condition",
    if_then_field: "consequence",
    if_alt_field: "alternative",
    elif_kind: None,

    while_kinds: &["while_statement"],
    while_cond_field: "condition",
    while_body_field: "body",

    loop_kinds: &[],
    loop_body_field: "",

    for_kinds: &["enhanced_for_statement"],
    for_pattern_field: "name",
    for_value_field: "value",
    for_body_field: "body",
    for_range_kind: "",
    for_clause_kind: "",

    return_kinds: &["return_statement"],
    throw_kinds: &["throw_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    break_texts: &[],
    continue_texts: &[],

    identifier_kinds: &["identifier"],

    // `switch_expression` covers both the colon/fallthrough form and the
    // `->` arrow form - both are modeled as mutually-exclusive arms (no
    // fallthrough), a deliberate v2 simplification; see the module docs.
    match_kinds: &["switch_expression"],
    match_subject_field: "condition",
    match_subject_kind: "",
    match_body_field: "body",
    match_body_kind: "",
    match_arm_kinds: &["switch_block_statement_group", "switch_rule"],
    match_arm_pattern_field: "",
    match_arm_pattern_kind: "switch_label",
    match_arm_pattern_multi: false,
    // A `case` label is a constant expression, never a destructuring
    // pattern - identifiers in it (an enum constant, say) are reads.
    match_arm_pattern_declares: false,
    match_arm_guard_field: "",
    match_pattern_guard_field: "",

    try_kinds: &["try_statement"],
    try_body_field: "body",
    try_body_kind: "",
    catch_kinds: &["catch_clause"],
    catch_body_field: "body",
    catch_body_kind: "",
    catch_param_kind: "catch_formal_parameter",
    catch_param_name_field: "name",
    catch_param_as_field: "",
    catch_param_as_pattern_kind: "",
    catch_param_alias_field: "",
    try_else_kind: "",
    try_else_body_field: "",
    finally_kind: "finally_clause",
    finally_body_field: "",
    finally_body_kind: "block",

    closure_kinds: &["lambda_expression"],
    closure_params_field: "parameters",
    closure_params_kind: "",
    closure_body_field: "body",
    closure_body_kind: "",
    closure_body_is_bare_children: false,
    // javac's real synthetic method naming (`lambda$owner$N`, dollar-
    // qualified, numbered per enclosing method here rather than per class).
    closure_name_format: "lambda${owner}${idx}",
};

pub(crate) static KOTLIN: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-kotlin",
    block_scoped: true,

    function_kinds: &["function_declaration"],
    function_name_field: "name",
    function_params_field: "",
    function_params_kind: "function_value_parameters",
    function_body_field: "",
    function_body_kind: "function_body",

    class_kinds: &["class_declaration"],
    class_name_field: "name",
    class_body_field: "",
    class_body_kind: "class_body",
    receiver_field: "",
    receiver_type_field: "",
    receiver_pointer_kind: "",

    block_kinds: &["block"],
    // `function_body` wraps either a `block` or a tail expression (`fun f()
    // = expr`); unwrapping it here lets `lower_module` decide which by
    // inspecting what comes out, exactly like `vikt-js`'s arrow expr-body.
    wrapper_kinds: &["function_body"],
    flatten_kinds: &[],

    binding_kinds: &["property_declaration"],
    binding_pattern_field: "",
    binding_pattern_kind: "variable_declaration",
    binding_value_field: "",
    binding_alt_field: "",
    binding_declarator_field: "",
    binding_declarator_name_field: "",
    binding_declarator_value_field: "",
    binding_kinds2: &[],
    binding_pattern_field2: "",
    binding_value_field2: "",

    assign_kinds: &["assignment"],
    compound_assign_kinds: &[],
    assign_operator_field: "operator",
    assign_declares: false,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call_expression"],
    call_callee_field: "",
    call_object_field: "",
    call_name_field: "",

    member_access_kinds: &["navigation_expression"],
    member_object_field: "",
    member_property_field: "",

    if_kinds: &["if_expression"],
    if_cond_field: "condition",
    if_then_field: "",
    if_alt_field: "",
    elif_kind: None,

    while_kinds: &["while_statement"],
    while_cond_field: "condition",
    while_body_field: "",

    loop_kinds: &[],
    loop_body_field: "",

    for_kinds: &["for_statement"],
    for_pattern_field: "",
    for_value_field: "",
    for_body_field: "",
    for_range_kind: "",
    for_clause_kind: "",

    return_kinds: &["return_expression"],
    throw_kinds: &["throw_expression"],
    break_kinds: &[],
    continue_kinds: &[],
    break_texts: &["break"],
    continue_texts: &["continue"],

    identifier_kinds: &["identifier"],

    match_kinds: &["when_expression"],
    // The subject sits in an unfielded `when_subject` wrapper
    // (`( expr )`); scanning its whole span for uses/calls finds the
    // wrapped expression regardless, no further unwrap needed.
    match_subject_field: "",
    match_subject_kind: "when_subject",
    // `when_entry` nodes are direct children of `when_expression` itself -
    // there is no separate arm-list wrapper to find by field or kind.
    match_body_field: "",
    match_body_kind: "",
    match_arm_kinds: &["when_entry"],
    match_arm_pattern_field: "condition",
    match_arm_pattern_kind: "",
    // `1, 2 -> ..` is two separate `condition:` fields on one `when_entry`,
    // not one node covering both.
    match_arm_pattern_multi: true,
    // A `when` condition is an ordinary boolean/equality expression, never
    // a destructuring pattern.
    match_arm_pattern_declares: false,
    match_arm_guard_field: "",
    match_pattern_guard_field: "",

    try_kinds: &["try_expression"],
    try_body_field: "",
    try_body_kind: "block",
    catch_kinds: &["catch_block"],
    catch_body_field: "",
    catch_body_kind: "block",
    // The bound name is a bare `identifier`, the first direct child of
    // `catch_block` - the exception *type*'s own identifier is nested one
    // level deeper inside `user_type`, so a direct-child kind search never
    // finds it by mistake.
    catch_param_kind: "identifier",
    catch_param_name_field: "",
    catch_param_as_field: "",
    catch_param_as_pattern_kind: "",
    catch_param_alias_field: "",
    try_else_kind: "",
    try_else_body_field: "",
    finally_kind: "finally_block",
    finally_body_field: "",
    finally_body_kind: "block",

    closure_kinds: &["lambda_literal"],
    closure_params_field: "",
    closure_params_kind: "lambda_parameters",
    closure_body_field: "",
    closure_body_kind: "",
    // `{ x -> stmt; stmt }` has no body wrapper at all - every named child
    // except `lambda_parameters` is the body, run in sequence.
    closure_body_is_bare_children: true,
    closure_name_format: "{owner}::{lambda#{idx}}",
};

/// Verified against real parse dumps from `tree-sitter-go` 0.25.0 (probed
/// directly, not guessed from the grammar source).
///
/// A `block`'s statements sit one level down, inside an unfielded
/// `statement_list` child (absent entirely when the block is empty) -
/// `block` is both a `wrapper_kinds` entry (unwraps to that child) and a
/// `block_kinds` entry in its own right (so an empty block, which has
/// nothing to unwrap to, still dispatches as an empty statement sequence
/// rather than falling through to `lower_module`'s expression-body
/// reading). See the field docs on `wrapper_kinds`/`block_kinds`.
///
/// Methods (`method_declaration`) are `function_kinds` alongside plain
/// `function_declaration`s - both field `name`/`parameters`/`body`
/// identically - but live as top-level siblings, not nested inside any
/// type body: `Type::method` naming instead comes from the function node's
/// own `receiver` field (`walk::receiver_owner`), never from an ancestor
/// search, so `class_kinds` stays empty here.
///
/// `var`/`const` declarations are two-level: `var_declaration`/
/// `const_declaration` (`flatten_kinds`, spliced into the surrounding
/// sequence - the parenthesized block form holds more than one
/// `var_spec`/`const_spec`, the bare form exactly one) each holding one or
/// more `var_spec`/`const_spec` (`binding_kinds`, `name`/`value` fielded
/// directly - `binding_kinds2`, since `short_var_declaration`'s `:=` and a
/// `select` arm's `receive_statement` need the *other* pair, `left`/
/// `right`, on the very same table). `const`'s implicit `iota`-repeat specs
/// (`One`/`Two` after `Zero = iota`) simply carry no `value` field at all -
/// handled by the same `None` path every binding already takes for a
/// value-less declaration.
///
/// `for_statement` is one grammar kind covering four loop shapes - see the
/// field docs on `for_range_kind`/`for_clause_kind` and `walk::lower_go_for`.
///
/// `expression_switch_statement` and `select_statement` share the ordinary
/// `match_kinds` treatment: a switch's `value` field is the discriminant
/// (absent entirely for a tagless `switch { case cond: .. }`, which
/// `match_subject_field`'s existing empty-is-fine handling already covers);
/// a select has no discriminant at all, so `match_subject_field` simply
/// never resolves for it either, same empty-is-fine path. `expression_case`
/// carries its label(s) in one `value` field (an `expression_list` wrapping
/// however many - `case 1, 2:` is one node, `match_arm_pattern_multi:
/// false`, mirroring Rust's `1 | 2`); `communication_case` has no `value`
/// field at all, so `match_arm_pattern_field` never finds anything on it -
/// every arm reads as a wildcard (`is_default`), and its own `communication`
/// child (the send/receive statement, unfielded to `match_arm_pattern_field`
/// so it is never excluded as a "label") is simply lowered as the arm's own
/// leading body statement instead. A `v := <-ch` receive is
/// `receive_statement`, `left`/`right` fielded exactly like
/// `short_var_declaration` - added to `binding_kinds` alongside it - so the
/// captured value is a real def, not an ambient read; a bare `<-done` (no
/// capture) has no `left` field, handled by the same value-only path a
/// value-less `var` spec takes. Type switches (`switch v := x.(type)`) are
/// a distinct grammar kind (`type_switch_statement`) from a value switch
/// and are deliberately unmodeled, like Java's C-style `for` - the generic
/// construct fallback still extracts every call inside, just without
/// per-branch exclusivity.
///
/// `panic`/`recover` are ordinary function calls - Go has no
/// exception-like node kind at all, so `throw_kinds` stays empty, same
/// rationale as Rust's (`Result`, not exceptions). A `defer`/`go` statement
/// is not itself a `call_kinds` node - it wraps exactly one call expression,
/// so both fall through to the generic construct fallback, which still
/// extracts that inner call as an ordinary `Call` node ahead of the
/// `defer`/`go` statement's own opaque unit: the deferred or launched call
/// is swept the same as any other, and denylisting still reaches it. This
/// frontend models no concurrency semantics for `go` beyond that - the
/// goroutine is not a separate flow, and nothing here reasons about when it
/// runs relative to the rest of the function.
///
/// `i++`/`i--` (`inc_statement`/`dec_statement`) field neither an operator
/// nor a left/right split - just a bare identifier and an anonymous `++`/
/// `--` token - so they are not `assign_kinds` here and fall through to the
/// generic construct fallback too: the identifier is recorded as a use, not
/// also a def, a documented v1 gap matching the existing "coarser, never
/// wrong" precedent elsewhere in this table.
pub(crate) static GO: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-go",
    block_scoped: true,

    function_kinds: &["function_declaration", "method_declaration"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_params_kind: "",
    function_body_field: "body",
    function_body_kind: "",

    class_kinds: &[],
    class_name_field: "",
    class_body_field: "",
    class_body_kind: "",
    receiver_field: "receiver",
    receiver_type_field: "type",
    receiver_pointer_kind: "pointer_type",

    block_kinds: &["block", "statement_list"],
    wrapper_kinds: &["block", "expression_statement"],
    flatten_kinds: &["var_declaration", "const_declaration"],

    binding_kinds: &[
        "short_var_declaration",
        "receive_statement",
        "var_spec",
        "const_spec",
    ],
    binding_pattern_field: "left",
    binding_pattern_kind: "",
    binding_value_field: "right",
    binding_alt_field: "",
    binding_declarator_field: "",
    binding_declarator_name_field: "",
    binding_declarator_value_field: "",
    binding_kinds2: &["var_spec", "const_spec"],
    binding_pattern_field2: "name",
    binding_value_field2: "value",

    assign_kinds: &["assignment_statement"],
    compound_assign_kinds: &[],
    assign_operator_field: "operator",
    assign_declares: false,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call_expression"],
    call_callee_field: "function",
    call_object_field: "",
    call_name_field: "",

    member_access_kinds: &["selector_expression"],
    member_object_field: "operand",
    member_property_field: "field",

    if_kinds: &["if_statement"],
    if_cond_field: "condition",
    if_then_field: "consequence",
    if_alt_field: "alternative",
    elif_kind: None,

    while_kinds: &[],
    while_cond_field: "",
    while_body_field: "body",

    loop_kinds: &[],
    loop_body_field: "body",

    for_kinds: &["for_statement"],
    for_pattern_field: "left",
    for_value_field: "right",
    for_body_field: "body",
    for_range_kind: "range_clause",
    for_clause_kind: "for_clause",

    return_kinds: &["return_statement"],
    throw_kinds: &[],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    break_texts: &[],
    continue_texts: &[],

    identifier_kinds: &["identifier"],

    match_kinds: &["expression_switch_statement", "select_statement"],
    match_subject_field: "value",
    match_subject_kind: "",
    match_body_field: "",
    match_body_kind: "",
    match_arm_kinds: &["expression_case", "default_case", "communication_case"],
    match_arm_pattern_field: "value",
    match_arm_pattern_kind: "",
    match_arm_pattern_multi: false,
    // A `case` value is an ordinary expression, never a destructuring
    // pattern.
    match_arm_pattern_declares: false,
    match_arm_guard_field: "",
    match_pattern_guard_field: "",

    // Go has no exception construct at all - see the module-level doc
    // comment above.
    try_kinds: &[],
    try_body_field: "",
    try_body_kind: "",
    catch_kinds: &[],
    catch_body_field: "",
    catch_body_kind: "",
    catch_param_kind: "",
    catch_param_name_field: "",
    catch_param_as_field: "",
    catch_param_as_pattern_kind: "",
    catch_param_alias_field: "",
    try_else_kind: "",
    try_else_body_field: "",
    finally_kind: "",
    finally_body_field: "",
    finally_body_kind: "",

    // `func_literal` (Go's closure form) is deliberately not modeled as its
    // own `FunctionIr` in this v1 - not required by any construct this
    // table needs to get right, and a lambda/func-literal's calls and
    // identifiers still get swept into whatever statement contains it via
    // the ordinary generic-construct fallback, same "coarser, never wrong"
    // treatment as Java's unmodeled `object_creation_expression` body.
    closure_kinds: &[],
    closure_params_field: "",
    closure_params_kind: "",
    closure_body_field: "",
    closure_body_kind: "",
    closure_body_is_bare_children: false,
    closure_name_format: "",
};
