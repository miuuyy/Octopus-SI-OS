use super::*;

pub(crate) fn isolated_thread_start_params(
    model: Option<String>,
    bound_instructions: String,
    native_affordances: Vec<DynamicToolSpec>,
) -> ThreadStartParams {
    ThreadStartParams {
        model,
        base_instructions: Some(String::new()),
        developer_instructions: Some(bound_instructions),
        environments: Some(Vec::<TurnEnvironmentParams>::new()),
        sandbox: Some(SandboxMode::ReadOnly),
        dynamic_tools: (!native_affordances.is_empty()).then_some(native_affordances),
        ..ThreadStartParams::default()
    }
}

pub(crate) fn non_empty_string_field(
    value: Option<&serde_json::Value>,
    field_path: &str,
) -> Result<Option<String>, InterfaceDriverError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(InterfaceDriverError::Protocol(format!(
            "{field_path} must be a non-empty string"
        ))),
    }
}

pub(crate) fn parse_reasoning_effort(
    effort: Option<&str>,
) -> Result<Option<ReasoningEffort>, InterfaceDriverError> {
    effort
        .map(|value| {
            value.parse::<ReasoningEffort>().map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "provider reasoning_effort '{value}' is invalid: {error}"
                ))
            })
        })
        .transpose()
}

pub(crate) fn parse_reasoning_summary(
    summary: Option<&str>,
) -> Result<ReasoningSummary, InterfaceDriverError> {
    match summary {
        Some("auto") => Ok(ReasoningSummary::Auto),
        Some("concise") => Ok(ReasoningSummary::Concise),
        Some("detailed") => Ok(ReasoningSummary::Detailed),
        Some("none") => Ok(ReasoningSummary::None),
        Some(value) => Err(InterfaceDriverError::Protocol(format!(
            "provider reasoning_summary '{value}' is invalid"
        ))),
        None => Err(InterfaceDriverError::FailedClosed(
            "provider interface declares no reasoning_summary; the app-server may not choose a hidden summary mode"
                .to_string(),
        )),
    }
}

pub(crate) fn prepend_context_block_to_turn_input(
    context_block: Option<String>,
    input: Vec<UserInput>,
) -> Vec<UserInput> {
    match context_block.filter(|block| !block.trim().is_empty()) {
        Some(block) => {
            let mut prepended = Vec::with_capacity(input.len() + 1);
            prepended.push(UserInput::Text {
                text: block,
                text_elements: Vec::new(),
            });
            prepended.extend(input);
            prepended
        }
        None => input,
    }
}

pub(crate) fn fresh_turn_start_params(
    thread_id: String,
    input: Vec<UserInput>,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<Option<String>>,
    reasoning_summary: ReasoningSummary,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
    turn_trigger: &str,
) -> TurnStartParams {
    TurnStartParams {
        thread_id,
        input,
        model: Some(model),
        effort: reasoning_effort,
        service_tier,
        summary: Some(reasoning_summary),
        additional_context: (!additional_context.is_empty())
            .then(|| additional_context.into_iter().collect()),
        turn_trigger: Some(turn_trigger.to_string()),
        ..TurnStartParams::default()
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedToolOutput {
    name: String,
    arrival_labels: Vec<String>,
}

impl RetainedToolOutput {
    pub(crate) fn from_private_config(
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<Self>, InterfaceDriverError> {
        let Some(value) = config.get("retained_tool_output") else {
            return Ok(None);
        };
        let declared: Self = serde_json::from_value(value.clone()).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "interface_driver_config.private_config.retained_tool_output is malformed: {error}"
            ))
        })?;
        let exact = |value: &str| !value.is_empty() && value.trim() == value;
        if !exact(&declared.name)
            || !declared.name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || declared.arrival_labels.is_empty()
            || !declared.arrival_labels.iter().all(|label| exact(label))
            || declared.arrival_labels.iter().collect::<BTreeSet<_>>().len() != declared.arrival_labels.len()
        {
            return Err(InterfaceDriverError::Protocol(
                "interface_driver_config.private_config.retained_tool_output needs a tool identifier name and distinct exact arrival labels"
                    .to_string(),
            ));
        }
        Ok(Some(declared))
    }

    pub(crate) fn opens(&self, label: &str) -> bool {
        self.arrival_labels.iter().any(|declared| declared == label)
    }

    pub(crate) fn refuse_operator_input<'a>(
        &self,
        declared_inputs: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), InterfaceDriverError> {
        let operator = declared_inputs
            .into_iter()
            .map(String::as_str)
            .chain([
                OPERATOR_MESSAGE_LABEL,
                OPERATOR_TURN_CONTROL_LABEL,
                octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL,
            ])
            .collect::<BTreeSet<_>>();
        match self.arrival_labels.iter().find(|label| operator.contains(label.as_str())) {
            Some(label) => Err(InterfaceDriverError::Protocol(format!(
                "retained tool output arrival '{label}' is operator input, which enters a turn as user input"
            ))),
            None => Ok(()),
        }
    }

    pub(crate) fn output(
        &self,
        namespace: &NativeToolNamespace,
        tool_names: &BTreeMap<String, String>,
        input: Vec<UserInput>,
    ) -> Result<TurnToolOutput, InterfaceDriverError> {
        if tool_names.contains_key(&self.name) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "retained tool output '{}' collides with a callable tool in namespace '{}'",
                self.name, namespace.name
            )));
        }
        let mut texts = input
            .into_iter()
            .map(|item| match item {
                UserInput::Text { text, text_elements } if text_elements.is_empty() && !text.is_empty() => Ok(text),
                _ => Err(InterfaceDriverError::FailedClosed(format!(
                    "retained tool output '{}' carries non-empty plain text only; the arrival holds other turn input",
                    self.name
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = match texts.len() {
            0 => {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "retained tool output '{}' has no text to carry",
                    self.name
                )));
            }
            1 => FunctionCallOutputBody::Text(texts.remove(0)),
            _ => FunctionCallOutputBody::ContentItems(
                texts
                    .into_iter()
                    .map(|text| FunctionCallOutputContentItem::InputText { text })
                    .collect(),
            ),
        };
        Ok(TurnToolOutput {
            name: self.name.clone(),
            namespace: Some(namespace.name.clone()),
            output,
            retain: true,
        })
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct HeldToolOutput {
    tool_output: TurnToolOutput,
}

pub(crate) fn held_turn_start(
    turn_input: &[UserInput],
    tool_output: Option<&TurnToolOutput>,
) -> Result<serde_json::Value, InterfaceDriverError> {
    match tool_output {
        Some(tool_output) if turn_input.is_empty() => serde_json::to_value(HeldToolOutput {
            tool_output: tool_output.clone(),
        }),
        Some(_) => {
            return Err(InterfaceDriverError::FailedClosed(
                "a retained tool output turn cannot also carry turn input".to_string(),
            ));
        }
        None => serde_json::to_value(turn_input),
    }
    .map_err(|error| {
        InterfaceDriverError::Protocol(format!("lifecycle start-turn input did not encode: {error}"))
    })
}

pub(crate) fn resumed_turn_start(
    turn_input: serde_json::Value,
) -> Result<(Vec<UserInput>, Option<TurnToolOutput>), InterfaceDriverError> {
    let malformed = |error: serde_json::Error| {
        InterfaceDriverError::Protocol(format!(
            "lifecycle start-turn continuation is malformed: {error}"
        ))
    };
    if turn_input.is_object() {
        let held: HeldToolOutput = serde_json::from_value(turn_input).map_err(malformed)?;
        if !held.tool_output.retain || held.tool_output.name.is_empty() {
            return Err(InterfaceDriverError::FailedClosed(
                "lifecycle start-turn continuation holds a tool output that is not a named retained output"
                    .to_string(),
            ));
        }
        return Ok((Vec::new(), Some(held.tool_output)));
    }
    Ok((serde_json::from_value(turn_input).map_err(malformed)?, None))
}

pub(crate) fn validate_model_service_tier(
    model: &octopus_appserver_proto::Model,
    tier: &str,
) -> Result<(), InterfaceDriverError> {
    if tier == "default" || model.service_tiers.iter().any(|entry| entry.id == tier) {
        return Ok(());
    }
    Err(InterfaceDriverError::FailedClosed(format!(
        "native model '{}' does not advertise service tier '{}'; available tiers: [{}]",
        model.model, tier, model.service_tiers.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>().join(", ")
    )))
}

pub(crate) fn validate_native_service_tier_response(
    requested: Option<&str>,
    reported: Option<&str>,
) -> Result<(), InterfaceDriverError> {
    if requested.is_some() && requested != reported {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "native provider did not retain selected service tier {requested:?}; active tier: {reported:?}"
        )));
    }
    Ok(())
}

pub(crate) fn native_turn_context(
    conversation: Option<String>,
    grant: Option<String>,
) -> BTreeMap<String, AdditionalContextEntry> {
    [
        ("octopus_bound_context", conversation, AdditionalContextKind::Untrusted),
        ("octopus_formed_policy", grant, AdditionalContextKind::Application),
    ]
    .into_iter()
    .filter_map(|(name, value, kind)| {
        value.filter(|value| !value.trim().is_empty())
            .map(|value| (name.to_string(), AdditionalContextEntry { value, kind }))
    })
    .collect()
}

pub(crate) fn encode_tool_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '-' => out.push_str("-h"),
            '.' => out.push_str("-d"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn decode_tool_name(encoded: &str) -> Result<String, InterfaceDriverError> {
    let mut out = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        if ch != '-' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('h') => out.push('-'),
            Some('d') => out.push('.'),
            other => {
                return Err(InterfaceDriverError::Protocol(format!(
                    "native tool name '{encoded}' has a malformed identifier escape (a '-' must be \
                     followed by 'h' or 'd'; got {other:?}) — failing closed, never routing a mangled \
                     tool name through the gate"
                )));
            }
        }
    }
    Ok(out)
}

pub(crate) fn extract_native_affordances_from_invocation(
    input: &InterfaceInvocationEnvelope,
    hidden_tools: &[String],
) -> Result<Vec<DynamicToolSpec>, InterfaceDriverError> {
    let arr = model_visible_affordances(input.interface_affordance_specs()?, hidden_tools)
        .map_err(InterfaceDriverError::Protocol)?;
    let mut specs = Vec::with_capacity(arr.len());
    for (i, t) in arr.iter().enumerate() {
        let name = t
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                InterfaceDriverError::Protocol(format!(
                    "invocation metadata interface_affordance_specs[{i}] has no non-empty 'name' (failing closed — a nameless \
                     native affordance would silently disappear from the interface-rendered list)"
                ))
            })?
            .to_string();
        let description = t
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_schema = match t.get("input_schema") {
            None => serde_json::json!({ "type": "object", "properties": {} }),
            Some(s) if s.is_object() => s.clone(),
            Some(_) => {
                return Err(InterfaceDriverError::Protocol(format!(
                    "invocation metadata interface_affordance_specs[{i}] ('{name}') has a non-object 'input_schema' (the native \
                     tool protocol requires a JSON-schema object) — failing closed"
                )));
            }
        };
        specs.push(DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: encode_tool_name(&name),
            description,
            input_schema,
            defer_loading: false,
        }));
    }
    Ok(specs)
}
