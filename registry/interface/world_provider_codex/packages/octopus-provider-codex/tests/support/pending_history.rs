#[tokio::test]
async fn pending_operator_history_pages_fail_closed_and_preserve_approval_replay() {
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    let turn = |id: &str, status: &str| json!({"id":id,"status":status,"items":[],"itemsView":"notLoaded","error":null});
    let page = |data: Vec<Value>, next: Option<&str>| json!({"data":data,"nextCursor":next,"backwardsCursor":null});
    let item = |owner: &str, id: &str| json!({"turnId":owner,"startedAtMs":5,"completedAtMs":null,"item":{"type":"contextCompaction","id":id}});
    let pending = page(vec![turn("pending", "inProgress")], None);
    let cases = vec![
        (vec![page(vec![turn("other", "completed")], Some("older")), pending.clone()],
            vec![page(vec![item("pending", "first")], Some("next")), page(vec![item("pending", "last")], None)], None),
        (vec![page(vec![], Some("loop")), page(vec![], Some("loop"))], vec![], Some("repeated a pagination cursor")),
        (vec![page(vec![turn("same", "completed"), turn("same", "completed")], None)], vec![], Some("repeated turn")),
        (vec![page(vec![], None)], vec![], Some("did not restore pending operator turn")),
        (vec![page(vec![turn("pending", "completed")], None)], vec![], Some("not in progress")),
        (vec![pending.clone()], vec![page(vec![item("wrong", "first")], None)], Some("expected 'pending'")),
        (vec![pending.clone()], vec![page(vec![], Some("loop")), page(vec![], Some("loop"))], Some("repeated a pagination cursor")),
        (vec![pending], vec![page(vec![item("pending", "same")], Some("next")), page(vec![item("pending", "same")], None)], Some("repeated item")),
    ];
    for (turn_pages, item_pages, expected_error) in cases {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("history.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap()).await.unwrap();
            let JSONRPCMessage::Request(init) = next_test_rpc(&mut socket).await else { panic!("initialize") };
            send_test_result(&mut socket, init.id, json!({"userAgent":"test", "codexHome":"/tmp"})).await;
            assert!(matches!(next_test_rpc(&mut socket).await, JSONRPCMessage::Notification(_)));
            socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
                "id":"approval", "method":"item/commandExecution/requestApproval", "params":{
                    "threadId":"exact", "turnId":"pending", "itemId":"command", "startedAtMs":1
                }
            }).to_string().into())).await.unwrap();
            for (method, pages) in [("thread/turns/list", turn_pages), ("thread/items/list", item_pages)] {
                let mut cursor = Value::Null;
                for response in pages {
                    let JSONRPCMessage::Request(request) = next_test_rpc(&mut socket).await else { panic!("page request") };
                    assert_eq!(request.method, method);
                    let params = request.params.unwrap();
                    assert_eq!(params["threadId"], "exact");
                    assert_eq!(params["cursor"], cursor);
                    if method == "thread/turns/list" {
                        assert_eq!(params["itemsView"], "notLoaded");
                        assert_eq!(params["sortDirection"], "desc");
                    } else {
                        assert_eq!(params["turnId"], "pending");
                        assert_eq!(params["sortDirection"], "asc");
                    }
                    cursor = response["nextCursor"].clone();
                    send_test_result(&mut socket, request.id, response).await;
                }
            }
            while socket.next().await.is_some() {}
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap(),
            },
            client_name:"test".into(), client_version:"0".into(), experimental_api:true,
            mcp_server_openai_form_elicitation:false, opt_out_notification_methods:Vec::new(), channel_capacity:8,
        }).await.unwrap();
        let driver = CodexAppServerInterfaceDriver::new_with_config(AppServerClient::Remote(client), None);
        let mut client = driver.client.lock().await;
        let result = driver.read_pending_operator_turn(&client, "exact", "pending").await;
        if let Some(error) = expected_error {
            assert!(result.unwrap_err().to_string().contains(error));
        } else {
            assert_eq!(result.unwrap().iter().map(|entry| (entry.item.id(), entry.started_at_ms, entry.completed_at_ms))
                .collect::<Vec<_>>(), vec![("first", Some(5), None), ("last", Some(5), None)]);
        }
        assert!(matches!(client.next_event().await, Some(AppServerEvent::ServerRequest(
            ServerRequest::CommandExecutionRequestApproval { request_id: RequestId::String(id), .. }
        )) if id == "approval"));
        drop(client);
        driver.shutdown().await.unwrap();
        server.await.unwrap();
    }
}
