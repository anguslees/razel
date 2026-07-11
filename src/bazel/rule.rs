use allocative::Allocative;

#[derive(Debug, Clone, Allocative)]
pub struct Rule {
    pub rule_class: String,
    pub name: String,
    // Add additional rule attributes here as needed later
}
