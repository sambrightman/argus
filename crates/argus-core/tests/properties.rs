//! Properties that must hold for any input, checked on generated ones
//! (proptest shrinks a failure to a minimal case).

use argus_core::config::Config;
use argus_core::identity::RepoId;
use argus_core::state::parse_duration;
use argus_core::status::version_cmp;
use proptest::prelude::*;

proptest! {
    /// A total order: antisymmetric, reflexive, and transitive on samples.
    #[test]
    fn version_order_is_consistent(a in "[a-z0-9.\\-]{0,12}", b in "[a-z0-9.\\-]{0,12}", c in "[a-z0-9.\\-]{0,12}") {
        prop_assert_eq!(version_cmp(&a, &b), version_cmp(&b, &a).reverse());
        prop_assert_eq!(version_cmp(&a, &a), std::cmp::Ordering::Equal);
        use std::cmp::Ordering::*;
        if version_cmp(&a, &b) != Greater && version_cmp(&b, &c) != Greater {
            prop_assert_ne!(version_cmp(&a, &c), Greater, "{} <= {} <= {}", a, b, c);
        }
    }

    /// Remote URLs come from repo configs: anything may appear.
    #[test]
    fn parsing_urls_never_panics(url in ".{0,80}") {
        let _ = RepoId::from_url(&url);
    }

    /// Parsed identities round-trip through their owner and name.
    #[test]
    fn scp_and_https_urls_agree(owner in "[a-zA-Z0-9][a-zA-Z0-9-]{0,15}", name in "[a-zA-Z0-9_][a-zA-Z0-9._-]{0,20}") {
        prop_assume!(!name.ends_with(".git") && name != "." && name != "..");
        let scp = RepoId::from_url(&format!("git@github.com:{owner}/{name}.git")).unwrap();
        let https = RepoId::from_url(&format!("https://github.com/{owner}/{name}")).unwrap();
        prop_assert_eq!(&scp, &https);
        prop_assert_eq!(scp.slug(), format!("{owner}/{name}"));
    }

    #[test]
    fn durations_parse_or_refuse_without_panicking(s in ".{0,12}") {
        let _ = parse_duration(&s);
    }

    #[test]
    fn durations_scale_by_unit(n in 1i64..10_000, unit in prop::sample::select(vec![("m", 60), ("h", 3600), ("d", 86_400), ("w", 604_800)])) {
        prop_assert_eq!(parse_duration(&format!("{n}{}", unit.0)), Some(n * unit.1));
    }

    /// `extend_<list>` appends to whatever the list holds, keeping order.
    #[test]
    fn extend_appends(base in prop::collection::vec("[a-z]{1,8}", 0..5), more in prop::collection::vec("[a-z]{1,8}", 0..5)) {
        let toml_list = |v: &[String]| format!("[{}]", v.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", "));
        let c = Config::parse(&format!("skip = {}\nextend_skip = {}", toml_list(&base), toml_list(&more))).unwrap();
        let mut want = base.clone();
        want.extend(more.clone());
        prop_assert_eq!(c.skip, want);
        // without the base, onto the defaults
        let c = Config::parse(&format!("extend_skip = {}", toml_list(&more))).unwrap();
        let mut want = Config::default().skip;
        want.extend(more);
        prop_assert_eq!(c.skip, want);
    }
}
