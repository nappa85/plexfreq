"""Qt 5.6 converts a whole QVariantMap/QVariantList on every QML read of a
Q_PROPERTY, so large collections must never be read per delegate, action or
binding, and must never live inside `backend.state`/`backend.cache`.

Only Session.qml may read `backend.items`/`backend.queueItems` (into its cached
`items` property and the playlist editor default); everything else uses
`music.items`, `backend.itemCount`/`queueCount` or the cache key invokables.
"""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
QML = sorted((ROOT / "app" / "qml").rglob("*.qml"))
QTEST_QML = sorted((ROOT / "tests").glob("*.qml"))
LISTS = re.compile(r"\bbackend\.(items|queueItems)\b")
# Rows formerly read from `state` (state.items, state.queue.items, with or
# without the `(state.queue || {})` guard) and the cache key collections.
MOVED = re.compile(
    r"state\.items\b|queue\)?\.items\b"
    r"|cache\.(readyKeys|goneKeys|pinnedKeys|pinnedGroups|readyBytes|errors)\b"
)


def reads(pattern):
    found = []
    for path in QML + QTEST_QML:
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if pattern.search(line):
                found.append((path.relative_to(ROOT).as_posix(), number))
    return found


class QmlReaderContract(unittest.TestCase):
    def test_qml_files_found(self):
        self.assertGreater(len(QML), 10)

    def test_only_session_reads_the_list_properties(self):
        outside = [r for r in reads(LISTS) if not r[0].endswith("shared/Session.qml")]
        self.assertEqual(outside, [], "backend.items/queueItems outside Session.qml")

    def test_session_reads_each_list_property_once_per_purpose(self):
        text = (ROOT / "app/qml/shared/Session.qml").read_text()
        # `property var items` (queueItems or items) and the editor default
        # (queueItems): three reads in all.
        self.assertEqual(
            sorted(m.group(1) for m in LISTS.finditer(text)),
            ["items", "queueItems", "queueItems"],
        )

    def test_moved_collections_are_not_read_from_state_or_cache(self):
        self.assertEqual(reads(MOVED), [])


if __name__ == "__main__":
    unittest.main()
