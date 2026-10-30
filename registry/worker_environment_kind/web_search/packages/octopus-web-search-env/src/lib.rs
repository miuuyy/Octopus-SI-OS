#[cfg(test)]
mod tests;
mod web_search;

pub async fn run_web_search_tool_process() -> Result<(), String> {
    octopus_tool_door_runtime::run_tool_process(std::sync::Arc::new(web_search::WebSearchRoom)).await
}
