import QtQuick 2.6
import PlexFreq 1.0

// The one row a library list holds while its content is the grid footer. A
// ListView without rows resets its scroll position whenever the footer grows
// (QQuickItemViewPrivate::layout), so the list needs a row; this one has no
// height and no visibility, and the list delegates render it as nothing.
EntryModel { entries: [{type: "spacer", title: "", artwork: ""}] }
