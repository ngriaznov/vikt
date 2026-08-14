// Fixture for --scope file: a hub function with real work in its body,
// called by three trivial wrappers, plus a private helper it calls and two
// unrelated leaves. Top-level `function name(){}` declarations calling each
// other by bare name are the one JS shape vikt-js resolves cleanly to a
// sibling FunctionIr (methods and arrow functions do not), so this fixture
// sticks to that shape throughout.

function helper(x) {
    return x * 2;
}

function hub(x) {
    let a = x + 1;
    let b = a * 2;
    let c = b - 3;
    return helper(c) + a;
}

function wrapperA(x) {
    return hub(x);
}

function wrapperB(x) {
    return hub(x);
}

function wrapperC(x) {
    return hub(x);
}

function leafOne(x) {
    return x + 1;
}

function leafTwo(x) {
    return x + 2;
}
