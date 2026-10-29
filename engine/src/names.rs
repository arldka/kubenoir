//! Deterministic identifiers. The simulation must be a pure function of its inputs, so there is
//! no randomness: names and uids derive from a hash and monotonic counters.

use serde_json::Value;

/// Same alphabet Kubernetes uses for generated name suffixes (no vowels, no easily-confused digits).
const SAFE_ALPHABET: &[u8] = b"bcdfghjklmnpqrstvwxz2456789";

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn safe_encode(mut n: u64, len: usize) -> String {
    let base = SAFE_ALPHABET.len() as u64;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(SAFE_ALPHABET[(n % base) as usize]);
        n /= base;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Hash seed derived from the scenario id.
pub fn seed_for(scenario_id: &str) -> u32 {
    (fnv1a(scenario_id.as_bytes()) & 0xffff_ffff) as u32
}

/// UUID-shaped uid for the `counter`-th object created in a cluster seeded with `seed`.
pub fn uid(seed: u32, counter: u64) -> String {
    format!("{seed:08x}-0000-4000-8000-{counter:012x}")
}

/// Generated 5-char pod name suffix (`web-7d9f8b6c5-<suffix>`).
pub fn pod_suffix(seed: u32, counter: u64) -> String {
    safe_encode(fnv1a(format!("pod/{seed}/{counter}").as_bytes()), 5)
}

/// `pod-template-hash` for a pod template. `Value` objects are key-sorted, so the JSON text is
/// canonical and equal templates always hash equally.
pub fn template_hash(template: &Value) -> String {
    safe_encode(fnv1a(template.to_string().as_bytes()), 9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn suffix_is_deterministic_and_well_formed() {
        assert_eq!(pod_suffix(7, 1), pod_suffix(7, 1));
        assert_ne!(pod_suffix(7, 1), pod_suffix(7, 2));
        assert_ne!(pod_suffix(7, 1), pod_suffix(8, 1));
        let s = pod_suffix(7, 1);
        assert_eq!(s.len(), 5);
        assert!(s.bytes().all(|b| SAFE_ALPHABET.contains(&b)), "{s}");
    }

    #[test]
    fn template_hash_ignores_key_order_but_not_content() {
        let a = json!({"spec": {"containers": [{"name": "a", "image": "x"}]}, "metadata": {"labels": {"app": "a"}}});
        let b = json!({"metadata": {"labels": {"app": "a"}}, "spec": {"containers": [{"image": "x", "name": "a"}]}});
        let c = json!({"metadata": {"labels": {"app": "a"}}, "spec": {"containers": [{"image": "y", "name": "a"}]}});
        assert_eq!(template_hash(&a), template_hash(&b));
        assert_ne!(template_hash(&a), template_hash(&c));
        assert_eq!(template_hash(&a).len(), 9);
    }

    #[test]
    fn uids_are_unique_per_counter_and_uuid_shaped() {
        assert_ne!(uid(1, 1), uid(1, 2));
        assert_ne!(uid(1, 1), uid(2, 1));
        let u = uid(0xdead, 255);
        assert_eq!(u, "0000dead-0000-4000-8000-0000000000ff");
        assert_eq!(u.len(), 36);
    }

    #[test]
    fn seed_depends_on_scenario_id() {
        assert_eq!(seed_for("a"), seed_for("a"));
        assert_ne!(seed_for("a"), seed_for("b"));
    }
}
