use schemars::{JsonSchema, schema_for};

pub fn surfaces() -> Vec<(&'static str, schemars::schema::RootSchema)> {
    fn entry<T: JsonSchema>(name: &'static str) -> (&'static str, schemars::schema::RootSchema) {
        (name, schema_for!(T))
    }
    vec![
        entry::<octopus_ledger::Movement>("movement"),
        entry::<octopus_interfaces::DeliveryVerdict>("delivery_verdict"),
        ("run_record", run_record_schema()),
        entry::<octopus_interfaces::GrantProjection>("grant_projection"),
        entry::<octopus_marks::RequiredVerifiedArtifact>("required_verified_artifact"),
        entry::<octopus_marks::Mark>("mark"),
        entry::<octopus_marks::MarkDescriptor>("mark_descriptor"),
        entry::<octopus_verifiers::VerifierDescriptor>("verifier_descriptor"),
    ]
}

fn run_record_schema() -> schemars::schema::RootSchema {
    let mut schema = schema_for!(octopus_run_record::RunRecord);
    let properties = &mut schema
        .schema
        .object
        .as_mut()
        .expect("RunRecord object schema")
        .properties;
    properties.insert(
        "status".into(),
        serde_json::from_value(serde_json::json!({
            "type": "string", "enum": ["running", "idle"]
        }))
        .expect("RunRecord status schema"),
    );
    let trace: schemars::schema::Schema = serde_json::from_value(serde_json::json!({
        "type": "string", "minLength": 1,
        "pattern": r"[^\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]"
    })).expect("RunRecord nonblank trace schema");
    properties.insert("trace".into(), trace.clone());
    let schemars::schema::Schema::Object(checkpoint) = schema
        .definitions
        .get_mut("CheckpointRecord")
        .expect("RunRecord checkpoint schema")
    else {
        panic!("RunRecord checkpoint is not an object schema");
    };
    checkpoint
        .object
        .as_mut()
        .expect("CheckpointRecord object schema")
        .properties
        .insert("trace".into(), trace);
    schema
}

pub fn schema_text(schema: &schemars::schema::RootSchema) -> String {
    let mut text = serde_json::to_string_pretty(schema).expect("schema serializes");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn schemas_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
    }

    #[test]
    fn committed_schemas_match_live_types() {
        let dir = schemas_dir();
        for (name, schema) in surfaces() {
            let path = dir.join(format!("{name}.schema.json"));
            let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "schema '{}' is not committed at {} ({e}); run `cargo run -p octopus-schemas --bin octopus-emit-schemas`",
                    name,
                    path.display()
                )
            });
            assert_eq!(
                committed,
                schema_text(&schema),
                "schema '{name}' drifted from the Rust type; regenerate with `cargo run -p octopus-schemas --bin octopus-emit-schemas`"
            );
        }
    }
}
