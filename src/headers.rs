pub fn prepare_upstream_headers(head: &str, traceparent: &str, client_ip: &str) -> String {
    let hop_by_hop = [
        "connection",
        "proxy-authorization",
        "proxy-authenticate",
        "proxy-connection",
        "te",
        "trailers",
        "transfer-encoding",
        "upgrade",
        "keep-alive",
    ];

    let filtered = head
        .lines()
        .filter(|line| {
            if !line.contains(':') {
                return true;
            }
            let name = line.split(':').next().unwrap_or("").trim().to_lowercase();
            !hop_by_hop.contains(&name.as_str())
        })
        .collect::<Vec<_>>();

    let mut result = filtered.join("\r\n");

    result.push_str(&format!("\r\nX-Forwarded-For: {client_ip}"));
    result.push_str(&format!("\r\ntraceparent: {traceparent}"));
    result.push_str("\r\n\r\n");

    result
}
