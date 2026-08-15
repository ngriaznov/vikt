//! Per-language node-kind/field-name vocabularies.
//!
//! [`GrammarTable`] is the only thing that varies between languages; the
//! walker in [`crate::walk`] is written once against it. A table entry names
//! a tree-sitter node kind and, where the grammar fields it, the field names
//! the walker reads off that kind — see the module docs on `walk` for how
//! each slot is used.

/// One language's node-kind/field-name vocabulary for the generic walker.
pub(crate) struct GrammarTable {
    /// Generator string recorded in a lowered function's provenance.
    pub generator: &'static str,

    /// Whether every `block_kinds` node introduces a fresh scope (Rust: `let`
    /// shadows per block) or the function has one flat scope throughout
    /// (Python: assignment binds for the whole function, `if`/`while`/`for`
    /// bodies do not nest scope).
    pub block_scoped: bool,

    pub function_kinds: &'static [&'static str],
    pub function_name_field: &'static str,
    pub function_params_field: &'static str,
    pub function_body_field: &'static str,

    /// A node whose named children are a sequence of statements.
    pub block_kinds: &'static [&'static str],
    /// Nodes that carry no meaning of their own and transparently unwrap to
    /// their `body` field, or their last named child when there is no such
    /// field (e.g. Rust's `expression_statement`, which wraps exactly the
    /// expression it terminates).
    pub wrapper_kinds: &'static [&'static str],

    /// A fresh-binding declaration form, e.g. Rust's `let`. Empty for a
    /// language where assignment is the only binding form (Python).
    pub binding_kinds: &'static [&'static str],
    pub binding_pattern_field: &'static str,
    pub binding_value_field: &'static str,

    pub assign_kinds: &'static [&'static str],
    pub compound_assign_kinds: &'static [&'static str],
    /// Whether a plain-variable assignment target declares (Python: the only
    /// binding form, reusing the slot on re-assignment) or must resolve an
    /// existing binding (Rust: `x = 1` is only legal after some `let x`).
    pub assign_declares: bool,
    pub assign_left_field: &'static str,
    pub assign_right_field: &'static str,

    pub call_kinds: &'static [&'static str],
    pub call_callee_field: &'static str,

    /// Method/attribute access, e.g. Rust's `field_expression`, Python's
    /// `attribute`. Used both to classify an assignment target as a state
    /// write and to extract a dotted callee name for method calls.
    pub member_access_kinds: &'static [&'static str],
    pub member_object_field: &'static str,
    pub member_property_field: &'static str,

    pub if_kinds: &'static [&'static str],
    pub if_cond_field: &'static str,
    pub if_then_field: &'static str,
    /// Field carrying the `else`/`elif` continuation(s). May be multi-valued
    /// (Python: zero or more `elif_clause` followed by an optional
    /// `else_clause`), so the walker reads it with `children_by_field_name`.
    pub if_alt_field: &'static str,
    /// A distinct "else if" node kind with its own condition/then fields
    /// (Python's `elif_clause`), reusing `if_cond_field`/`if_then_field`.
    /// `None` when the language chains via nested `if` inside its else
    /// wrapper instead (Rust).
    pub elif_kind: Option<&'static str>,

    pub while_kinds: &'static [&'static str],
    pub while_cond_field: &'static str,
    pub while_body_field: &'static str,

    /// A for-each loop: pattern/binding, iterated value, body. Neither
    /// language in this table has a C-style three-clause `for`.
    pub for_kinds: &'static [&'static str],
    pub for_pattern_field: &'static str,
    pub for_value_field: &'static str,
    pub for_body_field: &'static str,

    pub return_kinds: &'static [&'static str],
    pub throw_kinds: &'static [&'static str],
    pub break_kinds: &'static [&'static str],
    pub continue_kinds: &'static [&'static str],

    /// Leaf kind(s) that name a variable. Deliberately excludes a
    /// language's "this is a member/attribute name" leaf kind when that
    /// differs (Rust's `field_identifier`); where the grammar reuses the
    /// plain identifier kind for both (Python's `attribute`), the walker
    /// skips the `member_property_field` position explicitly instead.
    pub identifier_kinds: &'static [&'static str],
}

pub(crate) static RUST: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-rust",
    block_scoped: true,

    function_kinds: &["function_item"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_body_field: "body",

    block_kinds: &["block"],
    wrapper_kinds: &["expression_statement", "unsafe_block", "else_clause"],

    binding_kinds: &["let_declaration"],
    binding_pattern_field: "pattern",
    binding_value_field: "value",

    assign_kinds: &["assignment_expression"],
    compound_assign_kinds: &["compound_assignment_expr"],
    assign_declares: false,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call_expression"],
    call_callee_field: "function",

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

    for_kinds: &["for_expression"],
    for_pattern_field: "pattern",
    for_value_field: "value",
    for_body_field: "body",

    return_kinds: &["return_expression"],
    throw_kinds: &[],
    break_kinds: &["break_expression"],
    continue_kinds: &["continue_expression"],

    identifier_kinds: &["identifier"],
};

pub(crate) static PYTHON: GrammarTable = GrammarTable {
    generator: "vikt-ts/tree-sitter-python",
    block_scoped: false,

    function_kinds: &["function_definition"],
    function_name_field: "name",
    function_params_field: "parameters",
    function_body_field: "body",

    block_kinds: &["block"],
    wrapper_kinds: &["expression_statement", "else_clause"],

    binding_kinds: &[],
    binding_pattern_field: "",
    binding_value_field: "",

    assign_kinds: &["assignment"],
    compound_assign_kinds: &["augmented_assignment"],
    assign_declares: true,
    assign_left_field: "left",
    assign_right_field: "right",

    call_kinds: &["call"],
    call_callee_field: "function",

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

    for_kinds: &["for_statement"],
    for_pattern_field: "left",
    for_value_field: "right",
    for_body_field: "body",

    return_kinds: &["return_statement"],
    throw_kinds: &["raise_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],

    identifier_kinds: &["identifier"],
};
