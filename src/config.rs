use serde::Deserialize;

pub(crate) fn parse_config(contents: &str) -> Result<Config, serde_saphyr::Error> {
    serde_saphyr::from_str(contents)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) subscriptions: Vec<Subscription>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Subscription {
    pub(crate) provider: String,
    pub(crate) id: String,
    pub(crate) name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subscription() {
        let yaml = r#"
subscriptions:
  - provider: bubble
    id: EtyykC5BUJ
    name: Les 5 Terres
"#;

        let config = parse_config(yaml).unwrap();

        assert_eq!(config.subscriptions.len(), 1);

        let subscription = &config.subscriptions[0];
        assert_eq!(subscription.provider, "bubble");
        assert_eq!(subscription.id, "EtyykC5BUJ");
        assert_eq!(subscription.name, "Les 5 Terres");
    }

    #[test]
    fn rejects_unknown_subscription_field() {
        let yaml = r#"
subscriptions:
  - provider: bubble
    id: EtyykC5BUJ
    name: Les 5 Terres
    unexpected: true
"#;

        let result = parse_config(yaml);

        assert!(result.is_err());
    }

    #[test]
    fn rejects_missing_subscription_id() {
        let yaml = r#"
subscriptions:
  - provider: bubble
    name: Les 5 Terres
"#;

        let result = parse_config(yaml);

        assert!(result.is_err());
    }
    #[test]
    fn parses_anilist_subscription() {
        let yaml = r#"
subscriptions:
  - provider: anilist
    id: "154587"
    name: Frieren
"#;

        let config = parse_config(yaml).unwrap();
        let subscription = &config.subscriptions[0];

        assert_eq!(subscription.provider, "anilist");
        assert_eq!(subscription.id, "154587");
        assert_eq!(subscription.name, "Frieren");
    }
}
