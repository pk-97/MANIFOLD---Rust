//! Print every registered node with its ports and params, straight from the registry.

fn main() {
    print!("{}", manifold_nodes::catalog_gen::node_index());
}
