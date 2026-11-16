#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId, ToolDescriptorId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use serde_json::{json, Value};

use super::{desktop_action, desktop_operation, DesktopOperation, SystemGuiEnv};

const DOOR: &str = "system_gui";
const WORKER: &str = "system_worker_computer_use";
const KIND: &str = "desktop_session";
const AUTH_ENV: &str = "OCTOPUS_GUI_BACKEND_API_KEY";
static AUTH_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn app_resume_projects_exact_native_sources_and_materializes_original_windows() {
    use base64::Engine;
    use sha2::Digest;
    let descriptor: Value = serde_json::from_slice(include_bytes!("../../../../../interface/system_gui/desktop.json")).unwrap();
    let behavior: super::config::ApplicationBehavior = serde_json::from_value(descriptor["host"]["application_behavior"].clone()).unwrap();
    behavior.validate().unwrap();
    let protected = tempfile::tempdir().unwrap();
    let protected_root = protected.path().canonicalize().unwrap();
    let resource = protected_root.join(&behavior.instruction_resource.path);
    std::fs::create_dir_all(resource.parent().unwrap()).unwrap();
    std::fs::copy(registry_root().join("worker_environment_kind").join(&behavior.instruction_resource.path), &resource).unwrap();
    let mode = |mode: u32| std::fs::set_permissions(&resource, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(mode)).unwrap();
    mode(0o664);
    assert!(behavior.instruction_resource.read(&protected_root).unwrap_err().contains("protected regular file"));
    mode(0o644);
    let catalog = behavior.instruction_resource.read(&protected_root).unwrap();
    assert_eq!(catalog.aliases["com.apple.Music"], ["AppleMusic"]);
    assert!(catalog.suppressed_bundle_ids.contains("com.apple.iWork.Numbers"));
    assert!(catalog.resources["Clock"].text.contains("\"4000 seconds\" → 1 hour, 6 minutes, 40 seconds"));
    let mut changed_resource = behavior.instruction_resource.clone(); changed_resource.content_sha256 = "0".repeat(64);
    assert!(changed_resource.read(&protected_root).is_err());
    assert!(behavior.resolve("com.spotify.client").unwrap().full_ui.enforce_focus_before_enhanced);
    assert!(!behavior.resolve("com.spotify.client.extra").unwrap().full_ui.enforce_focus_before_enhanced);
    assert_eq!(behavior.resolve("com.apple.ScreenContinuity").unwrap().typing_interval_us,5000);
    assert_eq!(behavior.resolve("example.app").unwrap().typing_interval_us,250);
    assert!(behavior.resolve("com.apple.controlcenter").unwrap().suppress_menu_dismissal_on_window);
    assert!(!behavior.resolve("com.apple.controlcenter.extra").unwrap().suppress_menu_dismissal_on_window);
    assert!(behavior.defaults.menu_bar_presentation.omit_first_item);
    assert_eq!(behavior.defaults.accessibility_presentation.context_type, "full_ui");
    assert!(behavior.resolve("com.apple.iCal").unwrap().accessibility_presentation.omit_calendar_event_children);
    assert!(!behavior.resolve("com.apple.iCal.extra").unwrap().accessibility_presentation.omit_calendar_event_children);
    assert_eq!(behavior.defaults.menu_bar_presentation.excluded_title_substrings, ["Debug"]);
    assert!(behavior.resolve("com.apple.Safari").unwrap().focus_dom_field_by_click);
    assert!(!behavior.resolve("com.apple.Safari.extra").unwrap().focus_dom_field_by_click);
    assert_eq!(behavior.defaults.mouse_targeting, super::config::MouseTargeting::NativeHit);
    assert_eq!(behavior.resolve("com.openai.atlas.beta").unwrap().mouse_targeting, super::config::MouseTargeting::SuppliedWindow);
    assert_eq!(behavior.resolve("com.openai.other").unwrap().mouse_targeting, super::config::MouseTargeting::NativeHit);
    let mut invalid_behavior = descriptor["host"]["application_behavior"].clone();
    invalid_behavior["defaults"]["mouse_targeting"] = json!("foreground");
    assert!(serde_json::from_value::<super::config::ApplicationBehavior>(invalid_behavior).is_err());
    let mut ambiguous = behavior.clone(); ambiguous.profiles.push(ambiguous.profiles[1].clone());
    assert!(ambiguous.validate().is_err()); assert!(ambiguous.resolve("com.spotify.client").is_err());
    let root = tempfile::tempdir().unwrap();
    let input = envelope(root.path(), 1, "gui.app_resume", json!({"request_id":"request"}));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(3, 2).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let bytes = encoded.into_inner();
    let app = json!({"host_generation":"host", "app_instance_id":"app"});
    let windows: Vec<Value> = (1..=2).map(|id| json!({"window_id":format!("window-{id}"),"cg_window_id":id,
        "app":app,"title":null,"frame_screen_points":{"x":10.0,"y":20.0,"width":1.5,"height":1.0},
        "on_screen":true,"window_layer":0})).collect();
    let captures: Vec<Value> = windows.iter().enumerate().map(|(index, window)| json!({"app":app,"window":window,
        "source":{"primary_window_incarnation":format!("{}",index+1),"contributors":[{"kind":"application_window",
            "window_id":window["window_id"],"window_incarnation":format!("{}",index+1)}],
            "crop_screen_points":window["frame_screen_points"],"filter_mode":"desktop_independent_window"},
        "captured_at_unix_ms":1,"geometry":{"pixel_width":3,"pixel_height":2,"pixel_to_screen":[0.5,0.0,0.0,0.5,10.0,20.0],
        "filter_content_rect":{"x":0.0,"y":0.0,"width":1.5,"height":1.0},"point_pixel_scale":2.0},
        "image":{"image_id":format!("image-{index}"),"media_type":"image/png","width":3,"height":2,
        "byte_length":bytes.len(),"sha256":format!("{:x}",sha2::Sha256::digest(&bytes)),
        "data_base64":base64::engine::general_purpose::STANDARD.encode(&bytes)}})).collect();
    let accessibility = json!({"presentation":{"schema_version":1,"format":"macos_ax","context_type":"full_ui","mode":"full","base_revision":null,
            "text":"Window: \"Unknown\", App: Example.\nroot application\n","rendered_element_ids":["root"],"focused_element_id":null,"selected_text":null,"selected_element_ids":[]},
        "observation_id":"ax","app":app,"revision":1,"captured_at_unix_ms":1,"mode":"full",
        "context":{"root_kind":"application","root_element_id":"root","menu_bar_element_id":null,"display_omissions":[]},
        "base_revision":null,"nodes":[{"element_id":"root","parent_id":null,"window_id":null,
        "window_unavailable_reason":"ax_window_attribute_unavailable","role":"AXApplication","subrole":null,"title":null,
        "description":null,"value":null,"value_type":null,"value_input":null,"enabled":null,"focused":null,
        "frame_screen_points":null,"settable_attributes":[],"actions":[],"unavailable_attributes":[]}],
        "removed_ids":[],"complete":true,"omissions":[]});
    let mut body = json!({"request_id":"request","original_result":{"label":"gui.app_state.result","tool":"gui.app_state","correlation_id":"original","ok":true,"body":{"evidence":{"application":{
        "scope":{"host_generation":"host","worker_environment_id":"room","formation_pin":"pin"},
        "application":{"app":app,"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example",
        "process_id":123,"launch_time_unix_ms":null,"active":false,"hidden":false},
        "accessibility":accessibility,
        "windows":windows,"captures":captures,"capture_omissions":[],"preparation_effects":[],"control_generation":0,
        "primary_capture":{"observation_id":"ax","revision":1,"source_window_id":"window-2",
            "source_window_incarnation":"2","selection":"ax_focused_window","image_id":"image-1"}}}}}});
    body["original_result"]["body"]["evidence"]["application"]["instructions"] = json!({"bundle_id":"example.app",
        "bundle_path":"/Applications/Example.app","text":"Scoped app guidance",
        "content_sha256":format!("{:x}",sha2::Sha256::digest(b"Scoped app guidance"))});
    let state: super::protocol::application::State = serde_json::from_value(body["original_result"]["body"]["evidence"]["application"].clone()).unwrap();
    assert!(state.validate(&state.scope,&state.application.app).is_ok());
    assert_ne!(state.primary_capture.as_ref().unwrap().image_id.as_deref(),Some(state.captures[0].image.image_id.as_str()));
    use super::protocol::application::CaptureSelection;
    assert!(state.validate_capture_selection(CaptureSelection::AllWindows, true).is_ok());
    assert!(state.validate_capture_selection(CaptureSelection::Primary, true).is_err());
    let mut selected_capture = state.clone();
    selected_capture.captures.remove(0);
    selected_capture.capture_omissions.push(super::protocol::application::CaptureOmission {window_id:"window-1".into(),reason:"not_selected".into()});
    assert!(selected_capture.validate(&state.scope,&state.application.app).is_ok());
    assert!(selected_capture.validate_capture_selection(CaptureSelection::Primary, true).is_ok());
    assert!(selected_capture.validate_capture_selection(CaptureSelection::Primary, false).is_err());
    selected_capture.captures.clear(); selected_capture.primary_capture.as_mut().unwrap().image_id = None;
    selected_capture.capture_omissions.push(super::protocol::application::CaptureOmission {window_id:"window-2".into(),reason:"screenshots_not_requested".into()});
    assert!(selected_capture.validate(&state.scope,&state.application.app).is_ok());
    assert!(selected_capture.validate_capture_selection(CaptureSelection::Primary, false).is_ok());
    let state_tool = load_tool_descriptor_from_dir(&registry_root().join("tool"), "gui.app_state").unwrap().unwrap();
    let state_args = octopus_tool_registry::ToolArgumentValidator::compile(&state_tool.args_schema).unwrap();
    let state_arguments = json!({"app":{"kind":"bundle_id","value":"example.app"}});
    let make_state_action = |args:&Value| super::desktop_action_payload(DesktopOperation::AppState, "gui.app_state", args);
    assert_eq!(make_state_action(&state_arguments).unwrap()["capture_selection"], "all_windows");
    for selection in [json!("primary"),json!("all_windows"),json!(null),json!("first_window")] {
        let mut args = state_arguments.clone(); args["capture_selection"] = selection.clone();
        let valid = selection == "primary" || selection == "all_windows";
        assert_eq!(state_args.validate(&args).is_ok(), valid);
        assert_eq!(make_state_action(&args).is_ok(), valid);
        if valid { assert_eq!(make_state_action(&args).unwrap()["capture_selection"], selection); }
    }
    let mut primary = state.clone();
    primary.primary_capture.as_mut().unwrap().image_id = Some("image-0".into());
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    primary = state.clone(); primary.primary_capture.as_mut().unwrap().source_window_incarnation = "1".into();
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    primary = state.clone(); primary.primary_capture.as_mut().unwrap().revision += 1;
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    primary = state.clone(); primary.primary_capture.as_mut().unwrap().image_id = None;
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    primary.captures.pop(); primary.capture_omissions.push(super::protocol::application::CaptureOmission {window_id:"window-2".into(),reason:"screenshots_not_requested".into()});
    assert!(primary.validate(&state.scope,&state.application.app).is_ok());
    primary = state.clone();
    primary.captures[1].source.filter_mode = super::protocol::application::CaptureFilterMode::DisplayIncludingWindows;
    primary.captures[1].source.contributors.push(super::protocol::application::CaptureContributor::OwnedCursor {cg_window_id:99,host_generation:"host".into()});
    assert!(primary.validate(&state.scope,&state.application.app).is_ok());
    primary.captures[1].source.crop_screen_points.x += 1.0;
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    primary.captures[1].source.crop_screen_points.x -= 1.0;
    primary.captures[1].source.contributors[1] = super::protocol::application::CaptureContributor::OwnedCursor {cg_window_id:1,host_generation:"host".into()};
    assert!(primary.validate(&state.scope,&state.application.app).is_err());
    let mut missing = serde_json::to_value(&state).unwrap(); missing.as_object_mut().unwrap().remove("primary_capture");
    assert!(serde_json::from_value::<super::protocol::application::State>(missing).is_err());
    let mut guided = state.clone();
    assert!(guided.validate(&state.scope,&state.application.app).is_ok());
    guided.instructions.as_mut().unwrap().bundle_id = "foreign.app".into();
    assert!(guided.validate(&state.scope,&state.application.app).is_err());
    guided.instructions.as_mut().unwrap().bundle_id = state.application.bundle_id.clone();
    guided.instructions.as_mut().unwrap().text.push('!');
    assert!(guided.validate(&state.scope,&state.application.app).is_err());
    let original = super::app_authorization::OriginalCall { run:"run".into(), ee:"ee".into(), worker:InterfaceId::new(WORKER),
        door_interface:InterfaceId::new(DOOR), tool:"gui.app_state__task".into(), correlation_id:"original".into(),
        arguments:json!({"app":{"kind":"bundle_id","value":"example.app"},"include_screenshots":true}),
        source_movement_seq:5, source_artifact:input.artifact.clone() };
    let operator = super::app_authorization::OperatorRequest::new(original.clone(), state.scope.clone(),
        super::app_authorization::ApplicationTarget { bundle_id:"example.app".into(), bundle_path:"/Applications/Example.app".into(),
            display_name:"Example".into(), instance:None }, "gui.app_resume__task".into(), None, None).unwrap();
    let mut primary_original = original.clone(); primary_original.arguments["capture_selection"] = json!("primary");
    let primary_operator = super::app_authorization::OperatorRequest::new(primary_original, state.scope.clone(),
        operator.application.clone(), operator.resume.tool.clone(), None, None).unwrap();
    assert_ne!(operator.request_id, primary_operator.request_id);
    let eligibility: super::config::ApplicationPolicy = serde_json::from_value(descriptor["host"]["application_policy"].clone()).unwrap();
    eligibility.validate().unwrap();
    let policy_digest = eligibility.digest().unwrap();
    assert_eq!(policy_digest, format!("{:x}", sha2::Sha256::digest(octopus_descriptors::canonical_bytes(&descriptor["host"]["application_policy"]))));
    let native = super::protocol::NativeAppPolicy { decision:super::protocol::NativeAppPolicyDecision::Allowed,
        allow_persistent_approval:eligibility.allow_persistent_approval, risk:super::protocol::NativeAppRisk::High,
        warning_subtitle:Some(eligibility.high_risk_warning_subtitle.clone()), target:super::protocol::NativeAppPolicyTarget {
            bundle_id:operator.application.bundle_id.clone(), bundle_path:operator.application.bundle_path.clone(), display_name:operator.application.display_name.clone() } };
    let validate = |value: &super::protocol::NativeAppPolicy, digest: &str| super::app_authorization::validate_native_policy(
        &operator.application, Some(value), Some(digest), Some(&eligibility));
    assert!(validate(&native,&policy_digest).is_ok(), "high risk must not silently remove native persistence");
    assert!(validate(&native,&"0".repeat(64)).is_err());
    assert!(super::app_authorization::validate_native_policy(&operator.application,None,None,Some(&eligibility)).is_err());
    assert!(super::app_authorization::validate_native_policy(&operator.application,Some(&native),Some(&policy_digest),None).is_err());
    assert!(super::app_authorization::validate_native_policy(&operator.application,None,None,None).is_ok());
    for decision in [super::protocol::NativeAppPolicyDecision::Denied,super::protocol::NativeAppPolicyDecision::Forbidden] {
        let mut refused = native.clone(); refused.decision=decision; assert!(validate(&refused,&policy_digest).is_err());
    }
    let mut foreign = native.clone(); foreign.target.bundle_path="/Applications/Foreign.app".into();
    assert!(validate(&foreign,&policy_digest).is_err());
    let mut invalid=descriptor["host"]["application_policy"].clone(); invalid["enabled"]=json!("true");
    assert!(serde_json::from_value::<super::config::ApplicationPolicy>(invalid).is_err());
    use octopus_app_server_interface::{OperatorPermissionResponse, OperatorPermissionPersistence as Persistence};
    use super::app_authorization::{self as authorization, Decision};
    let legacy=serde_json::to_value(&operator).unwrap(); assert!(legacy.get("permission_offer").is_none());
    let exposed = authorization::OperatorRequest::new(original.clone(),state.scope.clone(),operator.application.clone(),
        operator.resume.tool.clone(),Some(native.clone()),None).unwrap();
    assert_eq!(serde_json::to_value(&exposed).unwrap()["native_policy"]["risk"],"high");
    assert_ne!(exposed.request_id,operator.request_id);
    let pending=authorization::PendingRequest { request:exposed.clone(),native_target_handle:"target".into(),
        operator_interface:InterfaceId::new("world_operator"),provider_interface:InterfaceId::new("world_provider"),
        native_policy:Some(native.clone()),native_policy_digest:Some(policy_digest) };
    pending.validate().unwrap();
    let mut retired=pending.clone(); retired.native_policy_digest=None; assert!(retired.validate().is_err());
    retired.native_policy=None; retired.request=operator.clone();
    retired.validate().unwrap();
    let omitted=serde_json::to_value(retired).unwrap(); assert!(omitted.get("native_policy").is_none() && omitted.get("native_policy_digest").is_none());
    assert_eq!(omitted["request"],legacy);
    let offered = authorization::permission_offer(pending.operator_interface.clone(), &native, true);
    assert_eq!(authorization::permission_offer(pending.operator_interface.clone(), &native, false).offered_choices
        .iter().map(|choice|(choice.decision,choice.persistence)).collect::<Vec<_>>(),
        [(Decision::Accept,Persistence::Session),(Decision::Decline,Persistence::Once)]);
    assert_eq!(offered.offered_choices.iter().map(|choice|(choice.decision,choice.persistence)).collect::<Vec<_>>(),
        [(Decision::Accept,Persistence::Session),(Decision::Decline,Persistence::Once),(Decision::Accept,Persistence::Always)]);
    let offered_request=authorization::OperatorRequest::new(original.clone(),state.scope.clone(),operator.application.clone(),
        operator.resume.tool.clone(),Some(native.clone()),Some(offered.clone())).unwrap();
    assert_ne!(offered_request.request_id,exposed.request_id);
    let mut changed=offered_request.clone(); changed.native_policy.as_mut().unwrap().warning_subtitle=Some("Changed warning".into());
    assert!(changed.validate().is_err());
    let response=OperatorPermissionResponse {request_id:offered_request.request_id.clone(),call_id:offered_request.call_id.clone(),
        decision:Decision::Accept,selected_persistence:Some(Persistence::Session)};
    assert!(response.validate(&offered_request.request_id,&offered_request.call_id,Some(&offered)).is_ok());
    let mut invalid=response.clone(); invalid.selected_persistence=Some(Persistence::Once);
    assert!(invalid.validate(&offered_request.request_id,&offered_request.call_id,Some(&offered)).is_err());
    invalid=response.clone(); invalid.decision=Decision::Cancel;
    assert!(invalid.validate(&offered_request.request_id,&offered_request.call_id,Some(&offered)).is_err());
    invalid.selected_persistence=None; assert!(invalid.validate(&offered_request.request_id,&offered_request.call_id,Some(&offered)).is_ok());
    let mut low=native.clone(); low.allow_persistent_approval=false;
    assert_eq!(authorization::permission_offer(pending.operator_interface.clone(),&low,true).offered_choices.len(),2);
    assert!(authorization::OperatorRequest::new(original.clone(),state.scope.clone(),operator.application.clone(),
        operator.resume.tool.clone(),Some(low),Some(offered.clone())).is_err());
    let mut removed=json!(pending); removed["consent_selection"]=json!({});
    assert!(serde_json::from_value::<authorization::PendingRequest>(removed).is_err());
    let mut removed=descriptor.clone(); removed["app_approval"]["application_consent"]=json!({});
    assert!(serde_json::from_value::<super::config::DesktopConfig>(removed).is_err());
    let canonical_root=root.path().canonicalize().unwrap();
    let private=canonical_root.join("permissions-private"); let shared=canonical_root.join("permissions-shared");
    for path in [&private,&shared] {std::fs::create_dir(path).unwrap(); std::fs::set_permissions(path,std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();}
    let mut decision=authorization::AdmittedDecision {request_id:offered_request.request_id.clone(),call_id:offered_request.call_id.clone(),
        decision:Decision::Decline,selected_persistence:Some(Persistence::Once),operator_interface:pending.operator_interface.clone(),operator_artifact:input.artifact.clone()};
    authorization::remember_permission(&private,Some(&shared),&offered_request,&decision).unwrap();
    assert_eq!(std::fs::read_dir(&private).unwrap().count(),0);
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(),0);
    assert!(authorization::lookup_permission(&private,None,&offered_request).unwrap().is_none());
    decision.decision=Decision::Accept;decision.selected_persistence=Some(Persistence::Session);
    authorization::remember_permission(&private,Some(&shared),&offered_request,&decision).unwrap();
    let mut fresh_scope=state.scope.clone();fresh_scope.host_generation="new-host".into();fresh_scope.formation_pin="new-pin".into();
    let fresh=authorization::OperatorRequest::new(original.clone(),fresh_scope,operator.application.clone(),operator.resume.tool.clone(),Some(native.clone()),Some(offered.clone())).unwrap();
    assert_eq!(authorization::lookup_permission(&private,Some(&shared),&fresh).unwrap().unwrap().decision.operator_artifact,input.artifact);
    let mut next_call=original.clone();next_call.run="another-run".into();
    let next=authorization::OperatorRequest::new(next_call,state.scope.clone(),operator.application.clone(),operator.resume.tool.clone(),Some(native.clone()),Some(offered)).unwrap();
    assert!(authorization::lookup_permission(&private,Some(&shared),&next).unwrap().is_none());
    decision.selected_persistence=Some(Persistence::Always);
    authorization::remember_permission(&private,Some(&shared),&offered_request,&decision).unwrap();
    assert_eq!(authorization::lookup_permission(&private,Some(&shared),&next).unwrap().unwrap().decision.selected_persistence,Some(Persistence::Always));
    let saved = super::application_settings(&shared,json!({"operation":"list"})).unwrap();
    assert_eq!(saved["schema_version"],1); assert_eq!(saved["applications"][0]["bundle_id"],"example.app");
    for invalid in [json!({"operation":"allow","bundle_id":"example.app"}),json!({"operation":"list","bundle_id":"example.app"}),json!({"operation":"remove"})] {
        assert!(super::application_settings(&shared,invalid).is_err());
    }
    assert_eq!(super::application_settings(&shared,json!({"operation":"remove","bundle_id":"example.app"})).unwrap()["removed"],true);
    assert!(authorization::lookup_permission(&private,Some(&shared),&next).unwrap().is_none());
    assert!(authorization::lookup_permission(&private,Some(&shared),&fresh).unwrap().is_some());
    decision.selected_persistence=Some(Persistence::Session);decision.decision=Decision::Decline;
    assert!(authorization::remember_permission(&private,Some(&shared),&offered_request,&decision).is_err());
    assert_eq!(authorization::lookup_permission(&private,None,&fresh).unwrap().unwrap().decision.decision,Decision::Accept);
    let mut resume = original.clone();
    resume.tool = operator.resume.tool.clone(); resume.correlation_id = operator.resume.correlation_id.clone();
    resume.arguments = json!({"request_id":operator.request_id}); resume.source_movement_seq = 8;
    let request = super::protocol::Request { schema_version:2, worker_environment_id:"room".into(), request_id:"resume-http".into(),
        tool:resume.tool.clone(), action:json!({"type":"app_resume","request_id":operator.request_id,
            "invocation":super::app_authorization::CallContext { scope:state.scope.clone(),call:resume,inbound_interface:InterfaceId::new("world_operator") }}) };
    let result = serde_json::to_value(super::app_authorization::ApplicationResult::Resumed { schema_version:3, operator_request:operator.clone(),
        original_result:octopus_tool_handlers::ToolResultArtifact::success(&original.tool,Some(original.correlation_id.clone()),
            json!({"accepted":true,"app_grant_request_id":operator.request_id,"evidence":{"application":state}})) }).unwrap();
    let source = super::app_authorization::live_app_source(&result, &request).unwrap().unwrap();
    assert_eq!(source.source, original.source());
    assert_eq!(source.application.launch_time_unix_ms, None);
    let evidence = json!({"status":"ready","permissions":{"screen_capture":true,"accessibility":true},
        "control":{"mode":"idle","owner":null,"generation":0},"screens":[],"observation":null,"effect":null});
    let projected = super::app_authorization::project_result(result.clone(),evidence,json!({}),&request).unwrap();
    let live = super::desktop_live_evidence(&projected);
    assert_eq!(live["presentation_intent"]["id"], 5);
    assert_eq!(live["applications"][0]["windows"][1]["window_id"], "window-2");
    assert_eq!(live["applications"][0]["windows"][1]["source_image"]["width"], 3);
    assert!(live.get("application").is_none());
    let serialized = serde_json::to_string(&live).unwrap();
    for forbidden in ["data_base64", "payload_base64", "media_type", "endpoint", "token", "Scoped app guidance"] { assert!(!serialized.contains(forbidden)); }
    let mut invalid = result.clone(); invalid["original_result"]["ok"] = json!(false);
    assert!(super::app_authorization::live_app_source(&invalid,&request).is_err());
    invalid = result.clone(); invalid["original_result"]["body"]["app_grant_request_id"] = json!(format!("sha256:{}","0".repeat(64)));
    assert!(super::app_authorization::live_app_source(&invalid,&request).is_err());
    let mut ax_state = state.clone();
    ax_state.accessibility.context.root_element_id = "foreign-root".into();
    assert!(ax_state.validate(&state.scope,&state.application.app).is_err());
    ax_state.accessibility.context.root_element_id = "root".into();
    ax_state.accessibility.nodes.push(serde_json::from_value(json!({"element_id":"button","parent_id":"root",
        "window_id":"window-1","window_unavailable_reason":null,"role":"AXButton","subrole":null,"title":null,
        "description":null,"value":null,"value_type":null,"enabled":true,"focused":false,"frame_screen_points":null,
        "settable_attributes":[],"actions":[],"unavailable_attributes":[]})).unwrap());
    assert!(ax_state.validate(&state.scope,&state.application.app).is_ok());
    ax_state.accessibility.nodes[1].window_id = Some("foreign-window".into());
    assert!(ax_state.validate(&state.scope,&state.application.app).is_err());
    ax_state.accessibility.nodes[1].window_id = None;
    ax_state.accessibility.nodes[1].window_unavailable_reason = Some("ax_window_attribute_unavailable".into());
    assert!(ax_state.validate(&state.scope,&state.application.app).is_ok());
    for (role, subrole, raw, value, input) in [
        ("AXTextField",None,None,None,json!({"kind":"string"})),
        ("AXSlider",None,Some("number"),Some("1.5"),json!({"kind":"float","format":"en_US decimal"})),
        ("AXCheckBox",Some("AXSwitch"),Some("number"),Some("1"),json!({"kind":"boolean"})),
        ("AXDateTimeArea",None,Some("date"),Some("09/09/2026"),json!({"kind":"date","format":"MM/dd/yyyy","date_components_mask":224})),
    ] {
        let node = &mut ax_state.accessibility.nodes[1];
        node.role = role.into(); node.subrole = subrole.map(str::to_owned); node.value_type = raw.map(str::to_owned);
        node.value = value.map(str::to_owned); node.value_input = Some(serde_json::from_value(input).unwrap());
        node.settable_attributes = vec!["AXValue".into()];
        assert!(ax_state.validate(&state.scope,&state.application.app).is_ok());
        ax_state.accessibility.nodes[1].settable_attributes.clear();
        assert!(ax_state.validate(&state.scope,&state.application.app).is_err());
    }
    ax_state.accessibility.nodes[1].settable_attributes = vec!["AXValue".into()];
    ax_state.accessibility.nodes[1].value_input.as_mut().unwrap().format = None;
    assert!(ax_state.validate(&state.scope,&state.application.app).is_err());
    let mut arguments = json!({"grant_request_id":operator.request_id,"app":state.application.app,
        "observation_id":state.accessibility.observation_id,"revision":state.accessibility.revision,
        "control_generation":state.control_generation,"window_id":null,"element_id":"button","value":"exact"});
    let set_value = super::protocol::application::ActionCall::parse("app_set_value", &arguments).unwrap();
    let mut scrolling = arguments.clone(); scrolling.as_object_mut().unwrap().remove("value");
    scrolling["window_id"] = json!("window-1"); scrolling["direction"] = json!("down"); scrolling["pages"] = json!(1.5);
    let descriptor = load_tool_descriptor_from_dir(&registry_root().join("tool"), "gui.app_scroll").unwrap().unwrap();
    let validator = octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap();
    for point in [false,true] {
        if point {
            scrolling.as_object_mut().unwrap().remove("element_id");
            scrolling["image_id"] = json!("image-0"); scrolling["x"] = json!(1.0); scrolling["y"] = json!(1.0);
        }
        assert!(validator.validate(&scrolling).is_ok());
        assert!(super::protocol::application::ActionCall::parse("app_scroll", &scrolling).is_ok());
        for (key,value) in [("pages",json!(0)),("window_id",json!(null)),("delta_y",json!(1))] {
            let mut invalid = scrolling.clone(); invalid[key] = value;
            assert!(validator.validate(&invalid).is_err());
            assert!(super::protocol::application::ActionCall::parse("app_scroll", &invalid).is_err());
        }
    }
    scrolling.as_object_mut().unwrap().remove("direction"); scrolling.as_object_mut().unwrap().remove("pages");
    scrolling["delta_x"] = json!(0); scrolling["delta_y"] = json!(-400);
    assert!(validator.validate(&scrolling).is_ok());
    assert!(super::protocol::application::ActionCall::parse("app_scroll", &scrolling).is_ok());
    scrolling["delta_y"] = json!(0);
    assert!(validator.validate(&scrolling).is_err());
    assert!(super::protocol::application::ActionCall::parse("app_scroll", &scrolling).is_err());
    arguments.as_object_mut().unwrap().remove("window_id");
    assert!(super::protocol::application::ActionCall::parse("app_set_value", &arguments).is_err());
    arguments["window_id"] = json!(null); arguments.as_object_mut().unwrap().remove("element_id");
    arguments.as_object_mut().unwrap().remove("value"); arguments["key_code"] = json!(0); arguments["modifiers"] = json!([]);
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_ok());
    arguments.as_object_mut().unwrap().remove("window_id");
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_err());
    arguments["window_id"] = json!("window-1");
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_ok());
    let descriptor = load_tool_descriptor_from_dir(&registry_root().join("tool"), "gui.app_key").unwrap().unwrap();
    let keys = octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap();
    assert!(keys.validate(&arguments).is_ok());
    arguments["key"] = json!("cmd+shift+p");
    assert!(keys.validate(&arguments).is_err());
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_err());
    arguments.as_object_mut().unwrap().remove("key_code"); arguments.as_object_mut().unwrap().remove("modifiers");
    assert!(keys.validate(&arguments).is_ok());
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_ok());
    arguments["modifiers"] = json!(null);
    assert!(keys.validate(&arguments).is_err());
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_err());
    arguments.as_object_mut().unwrap().remove("modifiers"); arguments.as_object_mut().unwrap().remove("key");
    assert!(keys.validate(&arguments).is_err());
    assert!(super::protocol::application::ActionCall::parse("app_key", &arguments).is_err());
    arguments["window_id"] = json!(null); arguments["element_id"] = json!("button");
    arguments["button"] = json!("left"); arguments["count"] = json!(1);
    let click = super::protocol::application::ActionCall::parse("app_click", &arguments).unwrap();
    arguments["image_id"] = json!("image-0"); arguments["x"] = json!(1.0); arguments["y"] = json!(1.0);
    assert!(super::protocol::application::ActionCall::parse("app_click", &arguments).is_err());
    arguments.as_object_mut().unwrap().remove("element_id");
    assert!(super::protocol::application::ActionCall::parse("app_click", &arguments).is_err());
    arguments["window_id"] = json!("window-1");
    assert!(super::protocol::application::ActionCall::parse("app_click", &arguments).is_ok());
    let mut outcome: super::protocol::application::ActionResult = serde_json::from_value(json!({
        "scope":state.scope,"call":click,"completed":true,
        "ax_effect":{"dispatch_state":"returned","mechanism":"ax_click","before_revision":click.target.revision,
            "native_error_code":0,"readback_matches":null,"readback_error":null},
        "input_delivery":null,"focus_effects":[],"cursor_milestones":[],"paste_evidence":null,"clipboard_failure":null,"menu_effects":[],
        "preparation_effects":[{"source_element_id":"button","affected_element_id":null,"mechanism":"ax_action",
            "name":"AXScrollToVisible","value":null,"dispatch_state":"returned","native_error_code":0}],"error":null})).unwrap();
    assert!(outcome.validate(&state.scope,&click).is_ok());
    let mut menu = outcome.clone(); menu.ax_effect = None; menu.preparation_effects.clear();
    menu.menu_effects.push(serde_json::from_value(json!({"reason":"requested","transition":"select",
        "source_element_id":"button","menu_bar_item_id":"button","menu_id":"menu","before_revision":click.target.revision,"changed":true})).unwrap());
    assert!(menu.validate(&state.scope,&click).is_ok());
    menu.menu_effects[0].source_element_id = "foreign".into();
    assert!(menu.validate(&state.scope,&click).is_err());
    menu.menu_effects[0].source_element_id = "button".into();
    menu.menu_effects[0].before_revision += 1;
    assert!(menu.validate(&state.scope,&click).is_err());
    menu.menu_effects[0].before_revision = click.target.revision;
    menu.ax_effect = outcome.ax_effect.clone();
    assert!(menu.validate(&state.scope,&click).is_err());
    menu.menu_effects[0].reason = "after_native_action".into(); menu.menu_effects[0].transition = "clear".into();
    assert!(menu.validate(&state.scope,&click).is_ok());
    menu.ax_effect = None;
    assert!(menu.validate(&state.scope,&click).is_err());
    let mut disclosure = outcome.clone(); disclosure.preparation_effects.clear();
    disclosure.call.operation = super::protocol::application::ActionParameters::AppSecondaryAction { element_id:"button".into(), action_id:"expand".into() };
    disclosure.ax_effect.as_mut().unwrap().mechanism = "ax_disclosing".into();
    disclosure.ax_effect.as_mut().unwrap().readback_matches = Some(true);
    assert!(disclosure.validate(&state.scope,&disclosure.call).is_ok());
    disclosure.ax_effect.as_mut().unwrap().mechanism = "ax_click".into();
    assert!(disclosure.validate(&state.scope,&disclosure.call).is_err());
    outcome.preparation_effects[0].source_element_id = "foreign".into();
    assert!(outcome.validate(&state.scope,&click).is_err());
    outcome.preparation_effects[0].source_element_id = "button".into();
    outcome.input_delivery = Some(super::protocol::application::InputDelivery {
        mechanism:"CGEventPostToPid".into(),dispatched_events:2,application_effect_verified:false });
    assert!(outcome.validate(&state.scope,&click).is_err());
    outcome.input_delivery = None; outcome.ax_effect = None; outcome.completed = false;
    outcome.error = Some(super::protocol::application::ActionError { code:"interrupted".into(),message:"cancelled after scrolling".into(),
        dispatch_attempted:true,dispatched_events:0,effect_uncertain:true,native_error_code:None });
    assert!(outcome.validate(&state.scope,&click).is_ok());
    outcome.error.as_mut().unwrap().dispatch_attempted = false;
    outcome.error.as_mut().unwrap().effect_uncertain = false;
    assert!(outcome.validate(&state.scope,&click).is_err());
    let mut field = outcome.clone(); field.call = set_value.clone();
    field.preparation_effects = vec![serde_json::from_value(json!({"source_element_id":"button","affected_element_id":"button",
        "mechanism":"CGEventPostToPid","name":"field_focus","value":null,"dispatch_state":"attempted",
        "native_error_code":null,"dispatched_events":1})).unwrap()];
    let error = field.error.as_mut().unwrap(); error.dispatch_attempted = true; error.effect_uncertain = true; error.dispatched_events = 1;
    assert!(field.validate(&state.scope,&set_value).is_ok());
    field.error.as_mut().unwrap().dispatched_events = 0;
    assert!(field.validate(&state.scope,&set_value).is_err());
    field.error.as_mut().unwrap().dispatched_events = 1;
    field.preparation_effects[0].affected_element_id = Some("foreign".into());
    assert!(field.validate(&state.scope,&set_value).is_err());
    let mut typing = click.clone();
    typing.target.window_id = None;
    typing.operation = super::protocol::application::ActionParameters::AppType { text:"x".repeat(2048) };
    assert!(typing.validate().is_ok());
    let mut coordinate = typing.clone();
    coordinate.operation = super::protocol::application::ActionParameters::AppMove { image_id:"image-1".into(),x:1.0,y:1.0 };
    assert!(coordinate.validate().is_err());
    outcome.call = typing.clone(); outcome.completed = true; outcome.error = None; outcome.preparation_effects.clear();
    outcome.input_delivery = Some(super::protocol::application::InputDelivery {
        mechanism:"CGEventPostToPid".into(),dispatched_events:8192,application_effect_verified:false });
    assert!(outcome.validate(&state.scope,&typing).is_ok());
    outcome.input_delivery.as_mut().unwrap().dispatched_events -= 1;
    assert!(outcome.validate(&state.scope,&typing).is_err());
    outcome.completed = false;
    outcome.error = Some(super::protocol::application::ActionError { code:"interrupted".into(),message:"cancelled during typing".into(),
        dispatch_attempted:true,dispatched_events:8191,effect_uncertain:true,native_error_code:None });
    assert!(outcome.validate(&state.scope,&typing).is_ok());
    outcome.input_delivery.as_mut().unwrap().dispatched_events = 8193;
    assert!(outcome.validate(&state.scope,&typing).is_err());
    let mut malformed = body.clone();
    malformed["original_result"]["body"]["evidence"]["application"]["captures"][1]["image"]["sha256"] = json!("wrong");
    assert!(super::materialize_image(&input, &mut malformed).is_err());
    super::materialize_image(&input, &mut body).unwrap();
    assert!(body.get("media").is_none());
    let original = &body["original_result"]["body"];
    assert_eq!(original["media"].as_array().unwrap().len(), 2);
    assert_eq!(original["evidence"]["application"]["instructions"]["text"], "Scoped app guidance");
    for index in 0..2 {
        let image = &original["evidence"]["application"]["captures"][index]["image"];
        let media = &original["media"][index];
        assert!(image.get("data_base64").is_none());
        assert_eq!(image["artifact"], media["artifact"]);
        assert_eq!(image["width"], 3);
        assert_eq!(image["height"], 2);
        assert_eq!(std::fs::read(media["artifact"]["payload_path"].as_str().unwrap()).unwrap(), bytes);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(media["payload_base64"].as_str().unwrap()).unwrap(), bytes);
    }
}

#[derive(Default)]
struct TestWorkerEnvironmentRecorder {
    live_states: std::sync::Mutex<Vec<Value>>,
    fail_lifecycle: AtomicBool,
    fail_live_state: AtomicBool,
}

impl TestWorkerEnvironmentRecorder {
    fn fail_next_evidence_writes(&self) {
        self.fail_lifecycle.store(true, Ordering::Release);
        self.fail_live_state.store(true, Ordering::Release);
    }
}

impl WorkerEnvironmentLifecycleRecorder for TestWorkerEnvironmentRecorder {
    fn record<'a>(
        &'a self,
        _formation: &'a WorkerEnvironmentFormation,
        _lifecycle: octopus_worker_env::WorkerEnvironmentLifecycle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_lifecycle.swap(false, Ordering::AcqRel) {
                Err("injected lifecycle write failure".to_string())
            } else {
                Ok(())
            }
        })
    }
}

impl WorkerEnvironmentLiveStateRecorder for TestWorkerEnvironmentRecorder {
    fn record_live_state<'a>(
        &'a self,
        _formation: &'a WorkerEnvironmentFormation,
        state: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_live_state.swap(false, Ordering::AcqRel) {
                return Err("injected live-state write failure".to_string());
            }
            self.live_states.lock().unwrap().push(state.clone());
            Ok(())
        })
    }
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn tool_ids() -> Vec<String> {
    [
        "gui.status",
        "gui.observe",
        "gui.screenshot",
        "gui.click",
        "gui.move",
        "gui.control",
        "gui.cancel",
        "gui.type",
        "gui.key",
        "gui.scroll",
        "gui.drag",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn raw_descriptor(id: &str) -> ToolDescriptor {
    load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
        .unwrap()
        .unwrap()
}

fn effective_descriptor(id: &str) -> ToolDescriptor {
    let mut descriptor = raw_descriptor(id);
    descriptor.interface = InterfaceId::new(DOOR);
    descriptor.process.private_read_subpaths.clear();
    descriptor.process.inherit_env = std::collections::BTreeSet::from([AUTH_ENV.into()]);
    descriptor
}

fn write_interface(path: &Path, action_url: &str, viewer_url: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("interface.json"),
        serde_json::to_vec_pretty(&json!({
            "id": DOOR,
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["gui"]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        path.join("desktop.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "action_url": action_url,
            "viewer_url": viewer_url,
            "auth_env_key": AUTH_ENV,
            "auth_header": "Authorization",
            "auth_value_prefix": "Bearer ",
            "headers": { "X-Desktop-Instance": "test-room" },
            "request_timeout_ms": 5000,
            "max_response_bytes": 1048576
        }))
        .unwrap(),
    )
    .unwrap();
}

fn formation(door_instance_dir: &Path) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    let registry =
        WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
            .unwrap();
    let tools = tool_ids();
    let hashes = tools
        .iter()
        .map(|tool| {
            let value = serde_json::to_value(effective_descriptor(tool)).unwrap();
            let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string();
            (tool.clone(), hash)
        })
        .collect::<BTreeMap<_, _>>();
    let process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), raw_descriptor(tool).process.bin))
        .collect::<BTreeMap<_, _>>();
    let kind_id = WorkerEnvironmentKindId::new(KIND);
    let kinds = vec![kind_id.clone()];
    let door = InterfaceId::new(DOOR);
    let worker = InterfaceId::new(WORKER);
    let formed = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: "desktop-test-door-hash",
            door_driver_ref: "system_gui",
            tool: "gui.status",
            tool_descriptor_hashes: &hashes,
            tool_process_bins: &process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_desktop_test",
            run: "run_desktop_test",
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: &BTreeMap::new(),
            door_instance_dir,
            ee_root_slot_allowance: None,
        })
        .unwrap();
    let (kind, _) = registry.get(&kind_id).unwrap();
    (formed, (*kind).clone())
}

fn envelope(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
) -> InterfaceInvocationEnvelope {
    envelope_with_tools(root, sequence, tool, arguments, tool_ids())
}

fn envelope_with_tools(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    tools: Vec<String>,
) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({
        "tool": tool,
        "arguments": arguments,
        "correlation_id": format!("call-{sequence}")
    }))
    .unwrap();
    let artifact_file = InterfaceArtifactFile::write(
        root.join(format!("inbound-{sequence}")),
        "tool.call",
        &payload,
    )
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let metadata = InvocationMetadata::new(GrantProjection {
        tools,
        worker_environment_kinds: vec![KIND.to_string()],
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_desktop_test"),
        ee: EeId::new("ee_desktop_test"),
        worker: InterfaceId::new(WORKER),
        checkpoint: CheckpointId::new(format!("cp-{sequence}")),
        interface: InterfaceId::new(DOOR),
        epoch: 0,
        run_record_hash: None,
    });
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: Some(metadata),
    }
}

async fn invoke(
    room: &SystemGuiEnv,
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
    recorder: &TestWorkerEnvironmentRecorder,
) -> Value {
    let invocation = ToolProcessInvocation {
        request_id: sequence,
        descriptor: effective_descriptor(tool),
        envelope: envelope(root, sequence, tool, arguments),
        source_movement_seq: sequence,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    let outputs = room
        .receive(invocation, recorder)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("desktop room invocation failed: {error}")
            }
        });
    serde_json::from_slice(&outputs[0].read_payload_bytes().unwrap()).unwrap()
}

struct MockBackend {
    action_url: String,
    viewer_url: String,
    handle: std::thread::JoinHandle<Vec<(BTreeMap<String, String>, Value)>>,
}

fn spawn_backend(replies: Vec<Value>) -> MockBackend {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let action_url = format!("http://{address}/v1/action");
    let viewer_url = format!("http://{address}/view");
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = read_request(&mut stream);
            requests.push((headers, body.clone()));
            let mut reply = reply;
            reply["worker_environment_id"] = body["worker_environment_id"].clone();
            reply["request_id"] = body["request_id"].clone();
            reply["operation"] = body["action"]["type"].clone();
            let bytes = serde_json::to_vec(&reply).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&bytes).unwrap();
            stream.flush().unwrap();
        }
        requests
    });
    MockBackend {
        action_url,
        viewer_url,
        handle,
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> (BTreeMap<String, String>, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let (header_end, content_length) = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                })
                .unwrap();
            if bytes.len() >= header_end + content_length {
                break (header_end, content_length);
            }
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end])
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap();
    (headers, body)
}

#[tokio::test]
async fn formed_desktop_room_drives_actions_returns_evidence_and_publishes_viewer() {
    let _env_lock = AUTH_ENV_LOCK.lock().await;
    let mut click_evidence = evidence();
    click_evidence["effect"] = json!({"screen_id":"screen-1","observation_id":"observation-1","control_generation":1,"events_posted":2});
    let mut screenshot_evidence = evidence();
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let image = encoded.into_inner();
    screenshot_evidence["screens"][0]["pixel_width"] = json!(1);
    screenshot_evidence["screens"][0]["pixel_height"] = json!(1);
    use base64::Engine;
    use sha2::Digest;
    screenshot_evidence["observation"] = json!({"id":"observation-1","screen":screenshot_evidence["screens"][0],"focus":null,"input_revision":"revision-1","captured_at_unix_ms":1,"image":{
        "media_type":"image/png","data_base64":base64::engine::general_purpose::STANDARD.encode(&image),"sha256":format!("{:x}",sha2::Sha256::digest(&image)),"byte_length":image.len(),"width":1,"height":1
    }});
    let backend = spawn_backend(vec![
        accepted(evidence()),
        accepted(click_evidence),
        accepted(screenshot_evidence),
        json!({"schema_version":2,"accepted":false,"evidence":null,"error":{"code":"focus_changed","message":"focus lost"}}),
        accepted(evidence()),
    ]);
    unsafe { std::env::set_var(AUTH_ENV, "desktop-secret") };
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir, &backend.action_url, &backend.viewer_url);
    let (formation, kind) = formation(&interface_dir);
    let room = SystemGuiEnv::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: None,
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let recorder = TestWorkerEnvironmentRecorder::default();

    let status = invoke(
        &room,
        root.path(),
        1,
        "gui.status",
        json!({}),
        &formation,
        &kind,
        &recorder,
    )
    .await;
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["body"]["evidence"]["status"], "ready");

    recorder.fail_next_evidence_writes();
    let click = invoke(
        &room,
        root.path(),
        2,
        "gui.click",
        json!({ "x": 12, "y": 64, "screen_id":"screen-1", "observation_id":"observation-1", "control_generation":1 }),
        &formation,
        &kind,
        &recorder,
    )
    .await;
    assert_eq!(click["ok"], false, "{click}");
    assert_eq!(
        click["error"]["details"]["operation_body"]["evidence"]["effect"]["events_posted"],
        json!(2)
    );
    assert_eq!(
        click["error"]["details"]["evidence_write_errors"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let screenshot = invoke(
        &room,
        root.path(),
        3,
        "gui.screenshot",
        json!({ "screen_id":"screen-1", "format": "png" }),
        &formation,
        &kind,
        &recorder,
    )
    .await;
    assert_eq!(screenshot["ok"], true, "{screenshot}");
    assert_eq!(
        screenshot["body"]["media"][0]["artifact"]["label"],
        "screen.captured"
    );

    let refused = invoke(
        &room,
        root.path(),
        4,
        "gui.type",
        json!({ "text": "must not fake success", "screen_id":"screen-1", "observation_id":"observation-1", "control_generation":1 }),
        &formation,
        &kind,
        &recorder,
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");
    let refused_error = refused["error"]["message"].as_str().unwrap();
    assert!(refused_error.contains("focus_changed"));
    assert!(!refused_error.contains("desktop-secret"));

    room.release(&formation, &kind)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("desktop room release failed: {error}")
            }
        });

    let requests = backend.handle.join().unwrap();
    assert_eq!(requests.len(), 5);
    for (index, (headers, body)) in requests.iter().enumerate() {
        assert_eq!(headers["authorization"], "Bearer desktop-secret");
        assert_eq!(headers["x-desktop-instance"], "test-room");
        assert_eq!(headers["x-octopus-call-sequence"], (index + 1).to_string());
        assert_eq!(
            body["worker_environment_id"],
            formation.worker_environment_id.as_str()
        );
    }
    assert_eq!(requests[0].1["action"]["type"], "status");
    assert_eq!(requests[1].1["action"]["type"], "click");
    assert_eq!(requests[2].1["action"]["type"], "screenshot");
    assert_eq!(requests[4].1["tool"], "worker_environment.release");
    assert_eq!(requests[4].1["action"]["type"], "release");

    let live_states = recorder.live_states.lock().unwrap();
    assert_eq!(live_states.len(), 3);
    let live = live_states.last().unwrap();
    assert_eq!(live["status"], "failed");
    assert_eq!(live["calls"], 4);
    assert_eq!(live["viewer"]["address"], backend.viewer_url);
    assert!(!live["detail"].as_str().unwrap().contains("desktop-secret"));
    unsafe { std::env::remove_var(AUTH_ENV) };
}

#[tokio::test]
async fn exact_clone_id_executes_copied_click_operation_and_stays_exact_in_evidence() {
    let _env_lock = AUTH_ENV_LOCK.lock().await;
    let mut click_evidence = evidence();
    click_evidence["effect"] = json!({"screen_id":"screen-1","observation_id":"observation-1","control_generation":1,"events_posted":2});
    let backend = spawn_backend(vec![accepted(click_evidence)]);
    unsafe { std::env::set_var(AUTH_ENV, "desktop-clone-secret") };
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir, &backend.action_url, &backend.viewer_url);
    let (mut formation, kind) = formation(&interface_dir);
    let exact_tool = "gui.click.run_42";
    let mut clone = effective_descriptor("gui.click");
    clone.id = ToolDescriptorId::new(exact_tool);
    formation.granted_tools.push(exact_tool.to_string());
    formation.granted_tools.sort();
    formation.tool_descriptor_hashes.insert(
        exact_tool.to_string(),
        blake3::hash(&octopus_descriptors::canonical_bytes(
            &serde_json::to_value(&clone).unwrap(),
        ))
        .to_hex()
        .to_string(),
    );
    let mut tools = tool_ids();
    tools.push(exact_tool.to_string());
    tools.sort();
    let room = SystemGuiEnv::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: None,
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let invocation = ToolProcessInvocation {
        request_id: 42,
        descriptor: clone,
        envelope: envelope_with_tools(
            root.path(),
            42,
            exact_tool,
            json!({"x": 7, "y": 9,"screen_id":"screen-1","observation_id":"observation-1","control_generation":1}),
            tools,
        ),
        source_movement_seq: 42,
        worker_environment: Some(formation),
        worker_environment_kind: Some(kind),
    };
    let recorder = TestWorkerEnvironmentRecorder::default();
    let outputs = room
        .receive(invocation, &recorder)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("desktop clone invocation failed: {error}")
            }
        });
    let response: Value =
        serde_json::from_slice(&outputs[0].read_payload_bytes().unwrap()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["tool"], exact_tool);
    assert_eq!(response["label"], format!("{exact_tool}.result"));

    let requests = backend.handle.join().unwrap();
    assert_eq!(requests[0].1["tool"], exact_tool);
    assert_eq!(requests[0].1["action"]["type"], "click");
    assert_eq!(requests[0].1["action"]["button"], "left");
    assert_eq!(requests[0].1["action"]["count"], 1);
    let observed = json!({"screen_id":"screen-1","observation_id":"observation-1","control_generation":1,"x":10,"y":20,"button":"right","count":2});
    let right = desktop_action(DesktopOperation::Click, "gui.click", &observed).unwrap();
    assert_eq!(right["button"], "right");
    assert_eq!(right["count"], 2);
    let mut moving = observed.clone();
    moving.as_object_mut().unwrap().remove("button");
    moving.as_object_mut().unwrap().remove("count");
    let moving = desktop_action(
        desktop_operation(&effective_descriptor("gui.move").handler_config, "gui.move")
            .unwrap_or_else(|_| panic!("gui.move descriptor operation is invalid")),
        "gui.move",
        &moving,
    )
    .unwrap();
    assert_eq!(moving["type"], "move");
    assert_eq!(moving["observation_id"], observed["observation_id"]);
    let mut invalid = observed.clone();
    invalid["count"] = json!(true);
    assert!(desktop_action(DesktopOperation::Click, "gui.click", &invalid).is_err());
    invalid = observed;
    invalid["button"] = json!("other");
    assert!(desktop_action(DesktopOperation::Click, "gui.click", &invalid).is_err());
    for handler_config in [
        json!({}),
        json!({"operation": 1}),
        json!({"operation": "unknown"}),
    ] {
        let error = desktop_operation(&handler_config, exact_tool).unwrap_err();
        let message = match error {
            ToolProcessError::Capability(message) | ToolProcessError::Unavailable(message) => {
                message
            }
        };
        assert!(message.contains(exact_tool), "{message}");
        assert!(message.contains("handler_config"), "{message}");
    }
    unsafe { std::env::remove_var(AUTH_ENV) };
}

fn evidence() -> Value {
    json!({"status":"ready","permissions":{"screen_capture":true,"accessibility":true},"control":{"mode":"idle","owner":null,"generation":1},"screens":[{"id":"screen-1","x":0,"y":0,"width":100,"height":100,"pixel_width":100,"pixel_height":100}],"observation":null,"effect":null})
}

fn accepted(evidence: Value) -> Value {
    json!({"schema_version":2,"accepted":true,"evidence":evidence,"error":null})
}

#[test]
fn native_host_rendezvous_is_private_exact_and_generation_owned() {
    use crate::native_connection::{HostPublication, PrivateConnection, CONNECTION_FILE};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut rooms = Vec::new();
    for name in ["system_gui_one", "system_gui_two"] {
        let private = root.path().join(name);
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = private.canonicalize().unwrap();
        let mut publication = HostPublication::prepare(&private, name).unwrap_or_else(|error| panic!("{error}"));
        assert!(HostPublication::prepare(&private, name).is_err());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        publication.publish(listener.local_addr().unwrap()).unwrap();
        let manifest = private.join(CONNECTION_FILE);
        assert!(PrivateConnection::read(&manifest, "system_gui_other").is_err());
        let connection = PrivateConnection::read(&manifest, name).unwrap_or_else(|error| panic!("{error}"));
        let linked = private.join("desktop-host/linked.json");
        std::os::unix::fs::symlink(&manifest, &linked).unwrap();
        assert!(PrivateConnection::read(&linked, name).is_err());
        rooms.push((private, publication, listener, connection));
    }
    assert!(rooms[0].3.action_url != rooms[1].3.action_url);
    assert!(rooms[0].3.host_generation != rooms[1].3.host_generation);
    assert!(rooms[0].3.token != rooms[1].3.token);
    let (private, publication, _, previous) = rooms.remove(0);
    drop(publication);
    assert!(!private.join(CONNECTION_FILE).exists());
    let next = HostPublication::prepare(&private, "system_gui_one").unwrap_or_else(|error| panic!("{error}"));
    assert!(next.connection.host_generation != previous.host_generation);
    assert!(next.connection.token != previous.token);
}

#[test]
fn remote_door_configuration_never_requires_a_native_manifest() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let interface = root.path().join("registry/interface").join(DOOR);
    write_interface(&interface, "https://desktop.example/action", "https://desktop.example/view");
    let private = root.path().join(DOOR);
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let roots = BTreeMap::from([
        ("activation_secrets_home".into(), private.canonicalize().unwrap()),
        ("registry_interface_root".into(), interface.parent().unwrap().canonicalize().unwrap()),
    ]);
    assert_eq!(crate::prepare_private_desktop_host(&roots).unwrap(), roots);
    assert!(!private.join(crate::native_connection::CONNECTION_FILE).exists());
    let config = crate::config::DesktopConfig::read(&interface.join("desktop.json")).unwrap();
    assert!(config.validate_process_inputs(&effective_descriptor("gui.status")).is_ok());
    assert!(config.validate_process_inputs(&raw_descriptor("gui.status")).is_err());
    let native = crate::config::DesktopConfig::read(&registry_root().join("interface/system_gui/desktop.json")).unwrap();
    assert!(native.validate_process_inputs(&raw_descriptor("gui.status")).is_ok());
    assert!(native.validate_process_inputs(&effective_descriptor("gui.status")).is_err());
}
