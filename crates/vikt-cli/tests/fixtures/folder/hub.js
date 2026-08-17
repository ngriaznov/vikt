// Cross-file hub, JavaScript side: jsHub does real work and calls jsHelper
// (its own file), and is called from wrapperA.js and wrapperB.js — two
// *different* files. Mirrors hub.py's shape exactly, so --scope repo has
// one same-language cross-file hub to rank in each of the two languages
// this folder mixes.

function jsHub(x) {
    let a = x + 1;
    let b = a * 2;
    let c = b - 3;
    return jsHelper(c) + a;
}

function jsHelper(x) {
    return x * 2;
}
