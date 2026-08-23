use crate::CloudObject;
use inkbridge_broker::{conflict_resolution_path, BROKER_PRODUCER};
use std::collections::{BTreeMap, BTreeSet};

const GENERATED_BY: &str = "inkbridge-generated-by";
const GENERATED_EVENT_ID: &str = "inkbridge-event-id";
const DOCUMENT_ID: &str = "inkbridge-document-id";
const KIND: &str = "inkbridge-kind";
const CONFLICT_EVENT_ID: &str = "inkbridge-conflict-event-id";
const RESOLUTION_KIND: &str = "conflict-resolution";

/// Returns one stable entry per unresolved conflict event. Preserved evidence
/// remains in storage after resolution; the broker-authenticated marker is what
/// removes the event from the active set.
pub(crate) fn unresolved_conflict_groups(
    objects: &[CloudObject],
    document_id: &str,
) -> BTreeSet<String> {
    let prefix = format!("Conflicts/{document_id}/");
    let mut groups = BTreeMap::<String, Vec<&CloudObject>>::new();
    for object in objects {
        let Some(remainder) = object.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some(event_segment) = remainder
            .split('/')
            .next()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        groups
            .entry(format!("{prefix}{event_segment}"))
            .or_default()
            .push(object);
    }

    groups
        .into_iter()
        .filter_map(|(group_path, objects)| {
            (!objects
                .iter()
                .any(|object| valid_resolution_marker(object, &group_path, document_id)))
            .then_some(group_path)
        })
        .collect()
}

fn valid_resolution_marker(object: &CloudObject, group_path: &str, document_id: &str) -> bool {
    object.path == format!("{group_path}/resolution.json")
        && object
            .metadata
            .get(GENERATED_BY)
            .is_some_and(|value| value == BROKER_PRODUCER)
        && object
            .metadata
            .get(DOCUMENT_ID)
            .is_some_and(|value| value == document_id)
        && object
            .metadata
            .get(KIND)
            .is_some_and(|value| value == RESOLUTION_KIND)
        && object
            .metadata
            .get(GENERATED_EVENT_ID)
            .is_some_and(|value| !value.trim().is_empty())
        && object
            .metadata
            .get(CONFLICT_EVENT_ID)
            .is_some_and(|value| conflict_resolution_path(document_id, value) == object.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(path: &str, metadata: BTreeMap<String, String>) -> CloudObject {
        CloudObject {
            path: path.to_owned(),
            generation: 1,
            size: 1,
            metadata,
        }
    }

    fn marker_metadata(document_id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (GENERATED_BY.to_owned(), BROKER_PRODUCER.to_owned()),
            (GENERATED_EVENT_ID.to_owned(), "resolution-1".to_owned()),
            (DOCUMENT_ID.to_owned(), document_id.to_owned()),
            (KIND.to_owned(), RESOLUTION_KIND.to_owned()),
            (CONFLICT_EVENT_ID.to_owned(), "event-1".to_owned()),
        ])
    }

    fn group_path(document_id: &str, event_id: &str) -> String {
        conflict_resolution_path(document_id, event_id)
            .strip_suffix("/resolution.json")
            .unwrap()
            .to_owned()
    }

    #[test]
    fn groups_preserved_evidence_as_one_conflict() {
        let document_id = "inkbridge-doc-v1-test";
        let prefix = group_path(document_id, "event-1");
        let groups = unresolved_conflict_groups(
            &[
                object(&format!("{prefix}/incoming.pdf"), BTreeMap::new()),
                object(&format!("{prefix}/current-supernote.json"), BTreeMap::new()),
            ],
            document_id,
        );
        assert_eq!(groups, BTreeSet::from([prefix]));
    }

    #[test]
    fn valid_resolution_marker_hides_but_does_not_require_removing_evidence() {
        let document_id = "inkbridge-doc-v1-test";
        let prefix = group_path(document_id, "event-1");
        let groups = unresolved_conflict_groups(
            &[
                object(&format!("{prefix}/incoming.pdf"), BTreeMap::new()),
                object(
                    &format!("{prefix}/resolution.json"),
                    marker_metadata(document_id),
                ),
            ],
            document_id,
        );
        assert!(groups.is_empty());
    }

    #[test]
    fn forged_or_incomplete_resolution_marker_is_ignored() {
        let document_id = "inkbridge-doc-v1-test";
        let prefix = group_path(document_id, "event-1");
        let mut forged = marker_metadata(document_id);
        forged.insert(GENERATED_BY.to_owned(), "device-adapter".to_owned());
        let groups = unresolved_conflict_groups(
            &[
                object(&format!("{prefix}/incoming.pdf"), BTreeMap::new()),
                object(&format!("{prefix}/resolution.json"), forged),
            ],
            document_id,
        );
        assert_eq!(groups, BTreeSet::from([prefix]));
    }

    #[test]
    fn marker_for_a_different_conflict_does_not_unblock_the_group() {
        let document_id = "inkbridge-doc-v1-test";
        let prefix = group_path(document_id, "event-1");
        let mut metadata = marker_metadata(document_id);
        metadata.insert(CONFLICT_EVENT_ID.to_owned(), "event-2".to_owned());
        let groups = unresolved_conflict_groups(
            &[
                object(&format!("{prefix}/incoming.pdf"), BTreeMap::new()),
                object(&format!("{prefix}/resolution.json"), metadata),
            ],
            document_id,
        );
        assert_eq!(groups, BTreeSet::from([prefix]));
    }
}
