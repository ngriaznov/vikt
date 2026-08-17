//! Contextual naming for anonymous closures: an arrow or unnamed `function`
//! borrows a name from whatever syntax binds it, falling back to an
//! enclosing-function-qualified or bare `<fn@LINE>` form only when nothing
//! does. See `vikt_js::lib`'s "Function naming" module docs.

fn names_of(source: &str, file: &str) -> Vec<String> {
    let lowered = vikt_js::lower_source(source, file).expect("fixture parses");
    lowered
        .functions
        .iter()
        .map(|f| f.id.name.clone())
        .collect()
}

/// `const createStyler = (open, close) => {...}` - the motivating case
/// (chalk's `source/index.js`, formerly `<fn@LINE>`).
#[test]
fn const_arrow_takes_its_declarator_name() {
    let names = names_of(
        "const createStyler = (open, close) => ({open, close});\n",
        "f.js",
    );
    assert!(
        names.contains(&"createStyler".to_owned()),
        "{names:?} must include the declarator's own name"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.contains("<fn@") || n.contains("<anon@"))
    );
}

/// `let x; x = function(){...}` - plain identifier assignment.
#[test]
fn plain_assignment_takes_its_target_name() {
    let names = names_of("let x;\nx = function () { return 1; };\n", "f.js");
    assert!(names.contains(&"x".to_owned()), "{names:?}");
}

/// `{ applyStyle: function(){...} }` - object-literal property.
#[test]
fn object_property_function_takes_its_key_name() {
    let names = names_of(
        "const styler = { applyStyle: function (s) { return s; } };\n",
        "f.js",
    );
    assert!(names.contains(&"applyStyle".to_owned()), "{names:?}");
}

/// `{ applyStyle() {...} }` - shorthand method, same shape as a property.
#[test]
fn object_shorthand_method_takes_its_key_name() {
    let names = names_of("const styler = { applyStyle(s) { return s; } };\n", "f.js");
    assert!(names.contains(&"applyStyle".to_owned()), "{names:?}");
}

/// `obj.applyStyle = function(){...}` - member assignment names the
/// function from the property being assigned, not the base object.
#[test]
fn member_assignment_takes_its_property_name() {
    let names = names_of(
        "const obj = {};\nobj.applyStyle = function (s) { return s; };\n",
        "f.js",
    );
    assert!(names.contains(&"applyStyle".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n == "obj"));
}

/// `class C { applyStyle = () => {...} }` - class field.
#[test]
fn class_field_arrow_takes_its_field_name() {
    let names = names_of("class C {\n  applyStyle = (s) => s;\n}\n", "f.js");
    assert!(names.contains(&"applyStyle".to_owned()), "{names:?}");
}

/// `class C { applyStyle() {...} }` - a class method has no `f.id` either,
/// same gap a class field closes.
#[test]
fn class_method_takes_its_method_name() {
    let names = names_of("class C {\n  applyStyle(s) { return s; }\n}\n", "f.js");
    assert!(names.contains(&"applyStyle".to_owned()), "{names:?}");
}

/// A named function expression keeps its own name even where an anonymous
/// one would borrow the assignment target's.
#[test]
fn named_function_expression_keeps_its_own_name() {
    let names = names_of("const wrapper = function helper() { return 1; };\n", "f.js");
    assert!(names.contains(&"helper".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n == "wrapper"));
}

/// A bare callback argument (`array.map(x => x)`) is truly anonymous: no
/// declarator, no assignment, no property. It borrows its enclosing named
/// function's name, qualified with its own line for uniqueness.
#[test]
fn bare_callback_argument_is_qualified_by_its_enclosing_function() {
    let src = "function outer(xs) {\n  return xs.map(x => x + 1);\n}\n";
    let names = names_of(src, "f.js");
    let qualified = names
        .iter()
        .find(|n| n.starts_with("outer.<anon@"))
        .unwrap_or_else(|| panic!("expected an outer.<anon@LINE> name, got {names:?}"));
    assert_eq!(qualified, "outer.<anon@2>");
}

/// An IIFE at module scope has no enclosing function to qualify with, so it
/// keeps the bare `<fn@LINE>` form unchanged from before contextual naming.
#[test]
fn top_level_iife_keeps_the_bare_line_qualified_form() {
    let src = "(function () {\n  console.log(\"boot\");\n})();\n";
    let names = names_of(src, "f.js");
    assert!(
        names.contains(&"<fn@1>".to_owned()),
        "{names:?} must still contain the bare top-level anonymous form"
    );
}

/// A destructuring declarator target has no single name to borrow; the
/// closure falls through to the enclosing-qualified/bare-line fallback
/// instead of misattributing one piece of the pattern.
#[test]
fn destructuring_target_does_not_borrow_a_partial_name() {
    let src = "const [a, b] = [() => 1, () => 2];\n";
    let names = names_of(src, "f.js");
    assert!(
        names.iter().all(|n| n != "a" && n != "b"),
        "a destructured target must never lend its name to a closure: {names:?}"
    );
}
