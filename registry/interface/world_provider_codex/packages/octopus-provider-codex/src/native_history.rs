use super::*;

pub(super) async fn read_native_turn(
    handle: &AppServerRequestHandle,
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<octopus_appserver_proto::Turn>, InterfaceDriverError> {
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut turns = BTreeSet::new();
    loop {
        let page = handle.request_typed::<ThreadTurnsListResponse>(ClientRequest::ThreadTurnsList {
            request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
            params: ThreadTurnsListParams {
                thread_id: thread_id.to_string(), cursor,
                limit: Some(100), sort_direction: Some(SortDirection::Desc),
                items_view: Some(TurnItemsView::NotLoaded),
            },
        }).await.map_err(typed_error)?;
        if page.next_cursor.as_ref().is_some_and(|next| !cursors.insert(next.clone())) {
            return Err(InterfaceDriverError::FailedClosed(
                "thread/turns/list repeated a pagination cursor".into(),
            ));
        }
        for turn in &page.data {
            if !turns.insert(turn.id.clone()) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "thread/turns/list repeated turn '{}'", turn.id,
                )));
            }
        }
        if let Some(turn) = page.data.into_iter().find(|turn| turn.id == turn_id) {
            return Ok(Some(turn));
        }
        cursor = page.next_cursor;
        if cursor.is_none() { return Ok(None); }
    }
}

pub(super) async fn read_native_turn_entries(
    handle: &AppServerRequestHandle,
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    turn_id: &str,
) -> Result<Vec<octopus_appserver_proto::ThreadItemEntry>, InterfaceDriverError> {
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut item_ids = BTreeSet::new();
    let mut items = Vec::new();
    loop {
        let page = handle.request_typed::<ThreadItemsListResponse>(ClientRequest::ThreadItemsList {
            request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
            params: ThreadItemsListParams {
                thread_id: thread_id.to_string(), turn_id: Some(turn_id.to_string()), cursor,
                limit: Some(100), sort_direction: Some(SortDirection::Asc),
            },
        }).await.map_err(typed_error)?;
        if page.next_cursor.as_ref().is_some_and(|next| !cursors.insert(next.clone())) {
            return Err(InterfaceDriverError::FailedClosed(
                "thread/items/list repeated a pagination cursor".into(),
            ));
        }
        for entry in page.data {
            if entry.turn_id != turn_id {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "thread/items/list returned item for turn '{}', expected '{turn_id}'", entry.turn_id,
                )));
            }
            if !item_ids.insert(entry.item.id().to_string()) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "thread/items/list repeated item '{}'", entry.item.id(),
                )));
            }
            items.push(entry);
        }
        cursor = page.next_cursor;
        if cursor.is_none() { return Ok(items); }
    }
}

pub(super) async fn read_native_items(
    handle: &AppServerRequestHandle,
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    turn_id: &str,
    item_ids: Vec<String>,
) -> Result<Vec<octopus_appserver_proto::ThreadItemEntry>, InterfaceDriverError> {
    let requested = item_ids.iter().cloned().collect::<BTreeSet<_>>();
    if !(1..=100).contains(&item_ids.len()) || requested.len() != item_ids.len() {
        return Err(InterfaceDriverError::FailedClosed(
            "thread/items/read requires 1 to 100 distinct item ids".into(),
        ));
    }
    let response = handle.request_typed::<octopus_appserver_proto::ThreadItemsReadResponse>(ClientRequest::ThreadItemsRead {
        request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        params: octopus_appserver_proto::ThreadItemsReadParams {
            thread_id: thread_id.to_string(), turn_id: turn_id.to_string(), item_ids,
        },
    }).await.map_err(typed_error)?;
    let mut returned = BTreeSet::new();
    for entry in &response.data {
        if entry.turn_id != turn_id {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "thread/items/read returned item for turn '{}', expected '{turn_id}'", entry.turn_id,
            )));
        }
        if !returned.insert(entry.item.id().to_string()) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "thread/items/read repeated item '{}'", entry.item.id(),
            )));
        }
    }
    if returned != requested {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "thread/items/read did not return exactly the requested items of turn '{turn_id}'",
        )));
    }
    Ok(response.data)
}
