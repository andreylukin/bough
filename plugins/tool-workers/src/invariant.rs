//! No runtime invariant: these are two model-facing CONSUMERS. Every relation they can violate
//! (bounds, seals, the spawner's chain) belongs to the `workers` seam, whose invariant runs over
//! their output; the tools themselves only translate arguments (§0.2).
