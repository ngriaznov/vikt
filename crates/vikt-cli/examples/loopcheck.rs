fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let lowered = vikt_jvm::lower_class(&bytes).unwrap();
        for ir in &lowered.functions {
            if ir.is_empty() {
                continue;
            }
            let g = vikt_core::Graph::build(ir);
            let back_edges: usize = (0..ir.nodes.len())
                .flat_map(|n| ir.nodes[n].succs.iter().map(move |&s| (n, s)))
                .filter(|&(n, s)| s <= n)
                .count();
            println!(
                "{:52} nodes={:<4} natural_loops={:<3} retreating_edges={}",
                ir.id.name,
                ir.nodes.len(),
                g.loops.len(),
                back_edges
            );
        }
    }
}
