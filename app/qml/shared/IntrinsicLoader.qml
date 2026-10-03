import QtQuick 2.6

Loader {
    // Loader assigns its explicit height back to the loaded item. Its actual
    // height therefore cannot be used as an input: use the intrinsic size.
    height: item ? item.implicitHeight : 0
    clip: true
}
