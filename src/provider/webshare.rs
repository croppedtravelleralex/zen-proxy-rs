use crate::pool::sha256_first8;

pub struct WebShareProvider {
    nodes: Vec<String>,
    creds: (String, String),
}

impl WebShareProvider {
    pub fn new(nodes: Vec<String>) -> Self {
        let creds = nodes
            .first()
            .and_then(|url| {
                let stripped = url.strip_prefix("socks5://")?;
                let (user_pass, _rest) = stripped.split_once('@')?;
                let (user, pass) = user_pass.split_once(':')?;
                Some((user.to_string(), pass.to_string()))
            })
            .unwrap_or_default();
        Self { nodes, creds }
    }

    pub fn from_file(path: &str) -> Self {
        let nodes = std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str::<Vec<String>>(&contents).ok())
            .unwrap_or_default();
        Self::new(nodes)
    }
}

impl crate::pool::NodeProvider for WebShareProvider {
    type NodeId = String;

    fn all_urls(&self) -> Vec<String> {
        self.nodes.clone()
    }

    fn id_for_url(&self, url: &str) -> String {
        sha256_first8(url)
    }

    fn name(&self) -> &'static str {
        "webshare"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::NodeProvider;

    #[test]
    fn test_new_extracts_creds() {
        let nodes = vec!["socks5://user1:pass1@1.2.3.4:1080".into()];
        let p = WebShareProvider::new(nodes);
        assert_eq!(p.creds, ("user1".into(), "pass1".into()));
        assert_eq!(p.nodes.len(), 1);
    }

    #[test]
    fn test_new_empty_nodes() {
        let p = WebShareProvider::new(vec![]);
        assert_eq!(p.creds, (String::new(), String::new()));
        assert!(p.all_urls().is_empty());
    }

    #[test]
    fn test_id_for_url_is_sha256_prefix() {
        let p = WebShareProvider::new(vec![]);
        let id = p.id_for_url("socks5://u:p@host:1080");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_name() {
        let p = WebShareProvider::new(vec![]);
        assert_eq!(p.name(), "webshare");
    }

    #[test]
    fn test_all_urls() {
        let nodes = vec!["socks5://a:b@x:1".into(), "socks5://c:d@y:2".into()];
        let p = WebShareProvider::new(nodes.clone());
        assert_eq!(p.all_urls(), nodes);
    }
}
