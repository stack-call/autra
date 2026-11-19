# API and Serialized Schema Are Versioned Separately

**Status:** accepted

A Builder API can change without breaking a stored Run, and a journal schema
can migrate without changing the Rust API. Coupling the two makes recovery
unnecessarily fragile.

We decided public APIs and serialized schemas have independent versions.
Durable records carry `schema_version`; incompatible journal changes require an
explicit migration path rather than deleting or repurposing fields.

Consequences: more metadata and migration work; old Runs remain explainable.
The 0.x API may evolve, but journal incompatibility must be announced.

Revisit when the first durable schema ships; the rule then becomes concrete.
