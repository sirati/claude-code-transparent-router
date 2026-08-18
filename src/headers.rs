use axum::http::header::{HeaderMap, HeaderName};

/// RFC 9110 hop-by-hop headers: connection-scoped, never forwarded.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authenticate",
    "proxy-authorization",
];

/// Owned by the transport on each leg: reqwest/axum set these from the actual
/// connection and body (which is forwarded byte-identical, so the recomputed
/// values match the originals).
const TRANSPORT_OWNED: [&str; 2] = ["host", "content-length"];

/// End-to-end headers verified safe to forward verbatim. The only way to build
/// one is [`end_to_end`], so a raw inbound map (with its hop-by-hop noise) can
/// never reach the upstream request by accident.
pub struct ForwardHeaders(HeaderMap);

impl ForwardHeaders {
    pub fn into_inner(self) -> HeaderMap {
        self.0
    }
}

/// Copy everything except hop-by-hop (including tokens named in `connection`)
/// and transport-owned headers. Multi-valued headers such as `anthropic-beta`
/// keep every value in order; nothing is collapsed, reordered, or added.
pub fn end_to_end(inbound: &HeaderMap) -> ForwardHeaders {
    let connection_named = connection_tokens(inbound);
    let mut out = HeaderMap::with_capacity(inbound.len());
    // `iter()` repeats the name for each value of a multi-valued header, so
    // `append` reproduces the original values in their original order.
    for (name, value) in inbound.iter() {
        if is_dropped(name.as_str(), &connection_named) {
            continue;
        }
        out.append(HeaderName::from(name), value.clone());
    }
    ForwardHeaders(out)
}

/// Filter an upstream response's headers for the inbound leg: same hop-by-hop
/// rules, and nothing of ours added.
pub fn response_headers(upstream: &HeaderMap) -> HeaderMap {
    end_to_end(upstream).into_inner()
}

fn is_dropped(name: &str, connection_named: &[String]) -> bool {
    HOP_BY_HOP.contains(&name)
        || TRANSPORT_OWNED.contains(&name)
        || connection_named.iter().any(|t| t == name)
}

fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn strips_hop_by_hop_and_transport_owned() {
        let mut inbound = HeaderMap::new();
        for (name, value) in [
            ("connection", "keep-alive"),
            ("transfer-encoding", "chunked"),
            ("host", "127.0.0.1:8787"),
            ("content-length", "42"),
            ("authorization", "Bearer sk-ant-test"),
            ("anthropic-version", "2023-06-01"),
        ] {
            inbound.append(name, HeaderValue::from_static(value));
        }
        let out = end_to_end(&inbound).into_inner();
        assert_eq!(out.len(), 2);
        assert_eq!(out["authorization"], "Bearer sk-ant-test");
        assert_eq!(out["anthropic-version"], "2023-06-01");
    }

    #[test]
    fn preserves_multi_valued_order() {
        let mut inbound = HeaderMap::new();
        inbound.append("anthropic-beta", HeaderValue::from_static("beta-one"));
        inbound.append("anthropic-beta", HeaderValue::from_static("beta-two"));
        let out = end_to_end(&inbound).into_inner();
        let values: Vec<_> =
            out.get_all("anthropic-beta").iter().map(|v| v.to_str().unwrap()).collect();
        assert_eq!(values, ["beta-one", "beta-two"]);
    }

    #[test]
    fn drops_headers_named_in_connection() {
        let mut inbound = HeaderMap::new();
        inbound.append("connection", HeaderValue::from_static("x-custom-hop"));
        inbound.append("x-custom-hop", HeaderValue::from_static("value"));
        inbound.append("x-app", HeaderValue::from_static("cli"));
        let out = end_to_end(&inbound).into_inner();
        assert!(out.get("x-custom-hop").is_none());
        assert_eq!(out["x-app"], "cli");
    }
}
