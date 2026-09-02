import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Simple Software Factory in the bar: who the bot is, whether the service is
// running, which repositories are watched (and by which agent), and the
// issues currently being worked on. Everything that needs typing goes through
// the Omarchy menu (`ssf-ui ...`) so it looks and behaves like the rest of the
// desktop; this panel is the read-out and the launch pad.
Panel {
  id: root
  moduleName: "ssf.factory"
  ipcTarget: "ssf.factory"

  property var status: ({})
  property bool loaded: false
  property bool cursorActive: false
  property int cursor: 0

  readonly property string botLogin: status && status.bot_login ? String(status.bot_login) : ""
  readonly property bool signedIn: botLogin !== ""
  readonly property bool serviceEnabled: status && status.service_enabled === true
  readonly property bool serviceActive: status && status.service_active === true
  readonly property string lastError: status && status.last_error ? String(status.last_error) : ""
  readonly property var repos: status && status.repos instanceof Array ? status.repos : []
  readonly property var issues: activeIssues()
  readonly property int refreshIntervalMs: Math.max(5, Number(setting("refreshIntervalSec", 30)) || 30) * 1000

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color barIconColor: serviceActive && signedIn ? barForeground : Qt.darker(barForeground, 1.55)

  // Cursor rows: repos first, then issues, then the action strip.
  readonly property int rowCount: repos.length + issues.length + 1

  function activeIssues() {
    var out = []
    for (var i = 0; i < repos.length; i++) {
      var list = repos[i].issues instanceof Array ? repos[i].issues : []
      for (var j = 0; j < list.length; j++) {
        if (list[j].active === true) out.push({ repo: String(repos[i].name || ""), issue: list[j] })
      }
    }
    return out
  }

  function heroMeta() {
    if (!loaded) return "Loading"
    if (!signedIn) return "Bot account not signed in"
    return "@" + botLogin
  }

  function heroDetail() {
    if (!loaded) return ""
    if (!serviceEnabled) return "Service disabled"
    if (!serviceActive) return "Service not running"
    if (lastError !== "") return "Last pass failed"
    var n = issues.length
    if (n === 0) return repos.length === 0 ? "No repositories watched" : "Watching " + repos.length + (repos.length === 1 ? " repository" : " repositories")
    return n + (n === 1 ? " issue" : " issues") + " in progress"
  }

  function refresh() {
    if (statusProc.running) return
    statusProc.running = true
  }

  function applyStatus(text) {
    try {
      var parsed = JSON.parse(text)
      root.status = parsed && typeof parsed === "object" ? parsed : {}
    } catch (e) {
      root.status = {}
    }
    root.loaded = true
    ensureCursor()
  }

  function run(command) {
    if (bar && typeof bar.run === "function") bar.run(command)
    else Quickshell.execDetached(["bash", "-lc", command])
    refreshLater.restart()
  }

  function shellQuote(value) {
    return "'" + String(value).replace(/'/g, "'\\''") + "'"
  }

  function toggleService() {
    run("ssf ui service toggle")
    root.close()
  }

  function editRepo(name) {
    run("ssf-ui edit-repo " + shellQuote(name))
    root.close()
  }

  function openIssue(entry) {
    if (!entry || !entry.issue) return
    if (entry.issue.worktree_id) run("ssf-ui open-workspace " + shellQuote(entry.issue.worktree_id) + " " + shellQuote(entry.issue.url || ""))
    else if (entry.issue.url) run("omarchy-launch-browser " + shellQuote(entry.issue.url))
    root.close()
  }

  function ensureCursor() {
    if (cursor < 0) cursor = 0
    if (cursor >= rowCount) cursor = Math.max(0, rowCount - 1)
  }

  function moveCursor(dy) {
    cursorActive = true
    cursor += dy
    ensureCursor()
  }

  function activateCursor() {
    if (!cursorActive) return
    if (cursor < repos.length) { editRepo(String(repos[cursor].name || "")); return }
    var idx = cursor - repos.length
    if (idx < issues.length) { openIssue(issues[idx]); return }
    run("ssf-ui add-repo")
    root.close()
  }

  onOpenedChanged: {
    if (opened) {
      cursorActive = false
      cursor = 0
      refresh()
    }
  }

  Component.onCompleted: refresh()

  Process {
    id: statusProc
    command: ["ssf", "status", "--json"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.applyStatus(text)
    }
  }

  Timer { interval: root.refreshIntervalMs; running: true; repeat: true; onTriggered: root.refresh() }
  Timer { interval: 5000; running: root.opened; repeat: true; onTriggered: root.refresh() }
  Timer { id: refreshLater; interval: 1200; repeat: false; onTriggered: root.refresh() }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    slotSize: Style.bar.iconSlot
    tooltipText: ""
    iconComponent: Component {
      Item {
        Text {
          anchors.centerIn: parent
          text: ""
          color: root.barIconColor
          font.family: root.fontFamily
          font.pixelSize: Style.bar.iconFont
        }
      }
    }
    onPressed: function(b) {
      if (b === Qt.RightButton) root.run("ssf-ui add-repo")
      else if (b === Qt.MiddleButton) root.refresh()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(400))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onMoveRequested: function(dx, dy) { if (dy !== 0) root.moveCursor(dy) }
      onActivateRequested: root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onTextKey: function(t) {
        if (t === "r") root.refresh()
        else if (t === "a") { root.run("ssf-ui add-repo"); root.close() }
        else if (t === "l") { root.run("ssf-ui logs"); root.close() }
        else if (t === "s") root.toggleService()
      }
    }

    Column {
      id: column
      width: parent.width
      spacing: Style.spacing.panelGap

      PanelHero {
        width: parent.width
        title: "Software Factory"
        meta: root.heroMeta()
        detail: root.heroDetail()
        foreground: root.foreground
        fontFamily: root.fontFamily
        iconComponent: Component {
          Text {
            text: ""
            color: root.serviceActive && root.signedIn ? root.foreground : root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.display
          }
        }
        trailingControl: Component {
          ToggleSwitch {
            checked: root.serviceEnabled
            foreground: root.foreground
            interactive: true
            onToggled: root.toggleService()
          }
        }
      }

      Text {
        visible: root.lastError !== ""
        width: parent.width
        text: root.lastError
        color: root.urgent
        opacity: 0.9
        wrapMode: Text.Wrap
        maximumLineCount: 3
        elide: Text.ElideRight
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      // ---- Sign-in nudge -------------------------------------------------
      Button {
        visible: root.loaded && !root.signedIn
        width: parent.width
        leftAlign: true
        bordered: true
        iconText: ""
        text: "Sign in the bot account"
        foreground: root.foreground
        fontFamily: root.fontFamily
        onClicked: { root.run("omarchy-launch-floating-terminal-with-presentation ssf-ui login"); root.close() }
      }

      PanelSeparator { foreground: root.foreground }

      // ---- Repositories --------------------------------------------------
      Column {
        width: parent.width
        spacing: Style.space(6)

        PanelSectionHeader {
          text: "REPOSITORIES"
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        Text {
          visible: root.loaded && root.repos.length === 0
          width: parent.width
          text: "Nothing watched yet. Add a repository and pick the agent that works it."
          color: root.dim
          wrapMode: Text.Wrap
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        Repeater {
          model: root.repos
          Button {
            required property var modelData
            required property int index
            width: parent.width
            leftAlign: true
            iconText: ""
            text: String(modelData.name || "")
            foreground: root.foreground
            fontFamily: root.fontFamily
            hasCursor: root.cursorActive && root.cursor === index
            tooltipText: "Change agent or stop watching"
            onClicked: root.editRepo(String(modelData.name || ""))
            onHovered: function(h) { if (h) { root.cursorActive = true; root.cursor = index } }

            Text {
              anchors.right: parent.right
              anchors.rightMargin: Style.spacing.controlPaddingX
              anchors.verticalCenter: parent.verticalCenter
              text: String(modelData.harness || "")
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
          }
        }

        Button {
          width: parent.width
          leftAlign: true
          iconText: ""
          text: "Watch a repository"
          foreground: root.foreground
          fontFamily: root.fontFamily
          hasCursor: root.cursorActive && root.cursor === root.repos.length + root.issues.length
          onClicked: { root.run("ssf-ui add-repo"); root.close() }
          onHovered: function(h) { if (h) { root.cursorActive = true; root.cursor = root.repos.length + root.issues.length } }
        }
      }

      PanelSeparator { foreground: root.foreground }

      // ---- Issues in progress -------------------------------------------
      Column {
        width: parent.width
        spacing: Style.space(6)

        PanelSectionHeader {
          text: "ISSUES IN PROGRESS"
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        Text {
          visible: root.loaded && root.issues.length === 0
          width: parent.width
          text: root.signedIn
            ? "Assign an issue to @" + root.botLogin + " and a workspace will appear here."
            : "Sign in first; issues assigned to the bot show up here."
          color: root.dim
          wrapMode: Text.Wrap
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        Repeater {
          model: root.issues
          Button {
            required property var modelData
            required property int index
            width: parent.width
            leftAlign: true
            iconText: ""
            text: "#" + String(modelData.issue.number) + "  " + String(modelData.issue.title || "")
            foreground: root.foreground
            fontFamily: root.fontFamily
            hasCursor: root.cursorActive && root.cursor === root.repos.length + index
            tooltipText: String(modelData.repo) + " · " + String(modelData.issue.prompts_sent || 0) + " prompts"
            onClicked: root.openIssue(modelData)
            onHovered: function(h) { if (h) { root.cursorActive = true; root.cursor = root.repos.length + index } }
          }
        }
      }

      PanelSeparator { foreground: root.foreground }

      // ---- Actions -------------------------------------------------------
      Row {
        width: parent.width
        spacing: Style.space(6)

        PanelActionButton {
          iconText: ""
          tooltipText: root.signedIn ? "Signed in as @" + root.botLogin + " — sign in again" : "Sign in the bot account"
          foreground: root.foreground
          fontFamily: root.fontFamily
          onClicked: { root.run("omarchy-launch-floating-terminal-with-presentation ssf-ui login"); root.close() }
        }
        PanelActionButton {
          iconText: ""
          tooltipText: "Logs"
          foreground: root.foreground
          fontFamily: root.fontFamily
          onClicked: { root.run("ssf-ui logs"); root.close() }
        }
        PanelActionButton {
          iconText: ""
          tooltipText: "Restart service"
          foreground: root.foreground
          fontFamily: root.fontFamily
          onClicked: root.run("ssf-ui service restart")
        }
        PanelActionButton {
          iconText: "󰋼"
          tooltipText: "Status and doctor"
          foreground: root.foreground
          fontFamily: root.fontFamily
          onClicked: { root.run("omarchy-launch-floating-terminal-with-presentation ssf-ui status"); root.close() }
        }
        PanelActionButton {
          iconText: ""
          tooltipText: "Refresh"
          foreground: root.foreground
          fontFamily: root.fontFamily
          onClicked: root.refresh()
        }
      }
    }
  }
}
