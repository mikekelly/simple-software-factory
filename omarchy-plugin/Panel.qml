import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Simple Software Factory in the bar: who the bot is, whether the service is
// running, which repositories are watched (and by which agent), and the agent
// sessions currently working. Everything comes from `ssf status --json`, which
// already joins ssf's view of each issue or PR with what Orca reports about
// the workspace (agent state, last message, activity); the widget never talks
// to Orca itself. Everything that needs typing goes through the Omarchy menu
// (`ssf-ui ...`) so it looks and behaves like the rest of the desktop; this
// panel is the read-out and the launch pad.
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
  // The wildcard allow-list is in effect on some repository: anyone with a
  // GitHub account can drive the agents. Shown as a warning until it is not.
  readonly property bool anyoneAllowed: status && status.anyone_allowed === true
  // Sessions whose harness sits at a login prompt: nothing reaches them
  // until a person signs the harness in, so they are shown as urgent.
  readonly property var blockedSessions: status && status.blocked_sessions instanceof Array ? status.blocked_sessions : []
  readonly property int blockedCount: blockedSessions.length
  readonly property var repos: status && status.repos instanceof Array ? status.repos : []
  readonly property bool orcaAvailable: !status || !status.orca || status.orca.available !== false
  readonly property var sessions: liveSessions()
  readonly property int workingCount: countState("working")
  readonly property int waitingCount: countState("waiting")
  readonly property int refreshIntervalMs: Math.max(5, Number(setting("refreshIntervalSec", 30)) || 30) * 1000

  // "3m ago" reads this instead of Date.now() so it keeps moving while the
  // panel sits open.
  property double nowMs: Date.now()

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color barIconColor: waitingCount > 0 || anyoneAllowed || blockedCount > 0 ? urgent
    : serviceActive && signedIn ? barForeground : Qt.darker(barForeground, 1.55)

  // Cursor rows: repos first, then sessions, then the action strip.
  readonly property int rowCount: repos.length + sessions.length + 1

  // Sessions worth a row: still active on GitHub, or retired but with the
  // workspace still around (the agent wrapping up after a close).
  function liveSessions() {
    var list = status && status.sessions instanceof Array ? status.sessions : []
    var out = []
    for (var i = 0; i < list.length; i++) {
      var s = list[i]
      if (!s || typeof s !== "object") continue
      if (s.active === true || (s.workspace && typeof s.workspace === "object")) out.push(s)
    }
    return out
  }

  function countState(state) {
    var n = 0
    for (var i = 0; i < sessions.length; i++) if (String(sessions[i].agent_state || "") === state) n++
    return n
  }

  function isPullRequest(s) { return String(s.kind || "") === "pull_request" }
  function isReviewer(s) { return String(s.kind || "") === "reviewer" }

  // Octicons: issue open/closed, pull request, merged, eye (reviewer session).
  function kindGlyph(s) {
    var state = String(s.github_state || "")
    if (isReviewer(s)) return "\uf441"
    if (isPullRequest(s)) return state === "merged" ? "\uf419" : "\uf407"
    return state === "closed" ? "\uf41d" : "\uf41b"
  }

  function kindColor(s) {
    var state = String(s.github_state || "")
    if (state === "closed" || state === "merged") return dim
    return foreground
  }

  function githubStateLabel(s) {
    var state = String(s.github_state || "")
    if (state === "merged") return "merged"
    if (state === "closed") return "closed"
    if (state === "open" && s.pr && s.pr.draft === true) return "draft"
    return ""
  }

  function isBlocked(s) { return !!(s && s.blocked && typeof s.blocked === "object") }

  function blockedLine(s) {
    var b = s.blocked
    var name = String(b.harness || "harness")
    if (name === "claude") name = "Claude Code"
    else if (name === "codex") name = "Codex"
    return name + " not signed in since " + (ago(b.since) || "?") + ": run " + String(b.fix || "its login")
  }

  function agentLabel(s) {
    if (isBlocked(s)) return "not signed in"
    switch (String(s.agent_state || "")) {
      case "working": return "working"
      case "waiting": return "waiting"
      case "done": return "done"
      case "open": return "idle"
      case "no-agent": return "no agent"
      case "no-workspace": return "no workspace"
      case "unbound": return "starting"
      case "unknown": return orcaAvailable ? "?" : "orca off"
      default: return String(s.agent_state || "")
    }
  }

  function agentColor(s) {
    if (isBlocked(s)) return urgent
    switch (String(s.agent_state || "")) {
      case "working": return foreground
      case "waiting": return urgent
      case "no-workspace": return urgent
      default: return dim
    }
  }

  function firstLine(text, max) {
    var flat = String(text || "").replace(/\s+/g, " ").trim()
    if (max && flat.length > max) return flat.slice(0, max - 1) + "…"
    return flat
  }

  // What the agent is up to: its last message, else the tool it is running;
  // for a blocked session, what a person has to do.
  function activityLine(s) {
    if (isBlocked(s)) return "\uf071  " + blockedLine(s)
    if (s.last_assistant_message) return firstLine(s.last_assistant_message, 200)
    if (s.tool) return "▸ " + firstLine(s.tool, 200)
    return ""
  }

  function ago(iso) {
    if (!iso) return ""
    var t = Date.parse(String(iso))
    if (isNaN(t)) return ""
    var secs = Math.max(0, Math.round((nowMs - t) / 1000))
    if (secs < 45) return "just now"
    if (secs < 5400) return Math.round(secs / 60) + "m ago"
    if (secs < 129600) return Math.round(secs / 3600) + "h ago"
    return Math.round(secs / 86400) + "d ago"
  }

  function shortBranch(s) {
    var b = String(s.branch || "")
    return b.length > 40 ? b.slice(0, 39) + "…" : b
  }

  // repo · branch · activity · who else is on it.
  function factsLine(s) {
    var parts = []
    var repo = String(s.repo || "")
    if (repo.indexOf("/") >= 0) repo = repo.slice(repo.indexOf("/") + 1)
    if (repo !== "") parts.push(repo)
    var state = githubStateLabel(s)
    if (state !== "") parts.push(state)
    var branch = shortBranch(s)
    if (branch !== "") parts.push(branch)
    var when = ago(s.last_activity_at || s.last_prompt_at)
    if (when !== "") parts.push(when)
    if (s.owner && s.id && s.owner !== s.id) parts.push("with " + String(s.owner).replace(/^.*#/, "#"))
    var subs = s.subscribers instanceof Array ? s.subscribers.length : 0
    if (subs > 0) parts.push(subs + (subs === 1 ? " subscriber" : " subscribers"))
    return parts.join(" · ")
  }

  function sessionTooltip(s) {
    var lines = [String(s.id || ""), "click: open the Orca workspace", "title or right-click: open on GitHub"]
    if (s.triggers instanceof Array && s.triggers.length > 0) lines.push("via " + s.triggers.join(", "))
    if (s.column) lines.push("column: " + String(s.column))
    lines.push(String(s.prompts_sent || 0) + " prompts")
    return lines.join("\n")
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
    if (blockedCount > 0) return blockedCount + (blockedCount === 1 ? " session" : " sessions") + " not signed in"
    if (anyoneAllowed) return "Open to anyone on GitHub"
    var n = sessions.length
    if (n === 0) return repos.length === 0 ? "No repositories watched" : "Watching " + repos.length + (repos.length === 1 ? " repository" : " repositories")
    var parts = [n + (n === 1 ? " session" : " sessions")]
    if (workingCount > 0) parts.push(workingCount + " working")
    if (waitingCount > 0) parts.push(waitingCount + " waiting")
    if (!orcaAvailable) parts.push("Orca not running")
    return parts.join(" · ")
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

  function openWorkspace(s) {
    if (!s) return
    if (s.worktree_id) run("ssf-ui open-workspace " + shellQuote(s.worktree_id) + " " + shellQuote(s.url || ""))
    else if (s.url) run("omarchy-launch-browser " + shellQuote(s.url))
    root.close()
  }

  function openGithub(s) {
    if (!s || !s.url) return
    run("omarchy-launch-browser " + shellQuote(s.url))
    root.close()
  }

  function cursorSession() {
    var idx = cursor - repos.length
    return cursorActive && idx >= 0 && idx < sessions.length ? sessions[idx] : null
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
    if (idx < sessions.length) { openWorkspace(sessions[idx]); return }
    run("ssf-ui add-repo")
    root.close()
  }

  onOpenedChanged: {
    if (opened) {
      cursorActive = false
      cursor = 0
      nowMs = Date.now()
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
  Timer { interval: 15000; running: root.opened; repeat: true; onTriggered: root.nowMs = Date.now() }

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
        else if (t === "g") { var s = root.cursorSession(); if (s) root.openGithub(s) }
        else if (t === "p") { root.run("omarchy-launch-floating-terminal-with-presentation ssf-ui peers"); root.close() }
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

      // ---- Wildcard allow-list warning ----------------------------------
      Text {
        visible: root.anyoneAllowed
        width: parent.width
        text: "\uf071  allowed_users is \"*\": anyone with a GitHub account can drive the agents. Set the logins with `ssf repo set <repo> --allowed-users` or `ssf config set daemon.allowed_users`."
        color: root.urgent
        wrapMode: Text.Wrap
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      // ---- Harness login gone --------------------------------------------
      Text {
        visible: root.blockedCount > 0
        width: parent.width
        text: "\uf071  " + (root.blockedCount === 1 ? "A session's harness" : root.blockedCount + " sessions' harnesses") + " sat at a login prompt (login expired or missing): nothing reaches " + (root.blockedCount === 1 ? "it" : "them") + " until the harness is signed in again. See the session rows for the command; ssf resumes them on its own afterwards."
        color: root.urgent
        wrapMode: Text.Wrap
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
            tooltipText: "Change agent, model or effort, or stop watching"
            onClicked: root.editRepo(String(modelData.name || ""))
            onHovered: function(h) { if (h) { root.cursorActive = true; root.cursor = index } }

            Text {
              anchors.right: parent.right
              anchors.rightMargin: Style.spacing.controlPaddingX
              anchors.verticalCenter: parent.verticalCenter
              text: [modelData.harness, modelData.model, modelData.effort, modelData.anyone_allowed === true ? "open to anyone" : ""].filter(function(x) { return !!x }).join(" · ")
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
          hasCursor: root.cursorActive && root.cursor === root.repos.length + root.sessions.length
          onClicked: { root.run("ssf-ui add-repo"); root.close() }
          onHovered: function(h) { if (h) { root.cursorActive = true; root.cursor = root.repos.length + root.sessions.length } }
        }
      }

      PanelSeparator { foreground: root.foreground }

      // ---- Sessions -----------------------------------------------------
      Column {
        width: parent.width
        spacing: Style.space(4)

        PanelSectionHeader {
          text: "SESSIONS"
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        Text {
          visible: root.loaded && root.sessions.length === 0
          width: parent.width
          text: root.signedIn
            ? "Assign an issue or PR to @" + root.botLogin + ", mention it, or request its review, and the agent session shows up here."
            : "Sign in first; the bot's agent sessions show up here."
          color: root.dim
          wrapMode: Text.Wrap
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        Text {
          visible: root.loaded && root.sessions.length > 0 && !root.orcaAvailable
          width: parent.width
          text: "Orca is not running, so agent states are unknown."
          color: root.dim
          wrapMode: Text.Wrap
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        Repeater {
          model: root.sessions
          SessionRow {
            required property var modelData
            required property int index
            width: parent.width
            session: modelData
            hasCursor: root.cursorActive && root.cursor === root.repos.length + index
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

  // One agent session: the item as a link with its GitHub state, the agent's
  // state, what it last said (or is running), and where it lives. Clicking the
  // row opens the Orca workspace; the title (or a right-click) opens GitHub.
  component SessionRow: BorderSurface {
    id: row
    property var session: null
    property bool hasCursor: false
    signal hovered(bool isHovered)

    readonly property bool hot: rowMouse.containsMouse || hasCursor
    readonly property string activity: root.activityLine(session || {})

    radius: Style.cornerRadius
    implicitHeight: body.implicitHeight + Style.spacing.controlPaddingY * 2
    color: rowMouse.pressed ? Style.pressedFillFor(root.foreground, Color.accent)
      : hot ? Style.hoverFillFor(root.foreground, Color.accent)
      : "transparent"
    Behavior on color { ColorAnimation { duration: 120 } }

    MouseArea {
      id: rowMouse
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      acceptedButtons: Qt.LeftButton | Qt.RightButton
      onClicked: function(mouse) {
        if (mouse.button === Qt.RightButton) root.openGithub(row.session)
        else root.openWorkspace(row.session)
      }
    }

    HoverHandler { onHoveredChanged: row.hovered(hovered) }

    PanelToolTip {
      visible: rowMouse.containsMouse && !titleMouse.containsMouse
      text: root.sessionTooltip(row.session || {})
      fontFamily: root.fontFamily
    }

    Column {
      id: body
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.leftMargin: Style.spacing.controlPaddingX
      anchors.rightMargin: Style.spacing.controlPaddingX
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(2)

      Item {
        width: parent.width
        implicitHeight: Math.max(kindIcon.implicitHeight, title.implicitHeight, stateBadge.implicitHeight)

        Text {
          id: kindIcon
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          text: root.kindGlyph(row.session || {})
          color: root.kindColor(row.session || {})
          font.family: root.fontFamily
          font.pixelSize: Style.font.icon
        }

        Text {
          id: title
          anchors.left: kindIcon.right
          anchors.leftMargin: Style.spacing.controlGap
          anchors.right: stateBadge.left
          anchors.rightMargin: Style.spacing.controlGap
          anchors.verticalCenter: parent.verticalCenter
          text: "#" + String(row.session ? row.session.number : "") + "  " + String(row.session ? row.session.title || "" : "")
          color: root.kindColor(row.session || {})
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          font.underline: titleMouse.containsMouse
          elide: Text.ElideRight

          MouseArea {
            id: titleMouse
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            acceptedButtons: Qt.LeftButton
            onClicked: root.openGithub(row.session)
          }

          PanelToolTip {
            visible: titleMouse.containsMouse
            text: "Open on GitHub"
            fontFamily: root.fontFamily
          }
        }

        Text {
          id: stateBadge
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          text: root.agentLabel(row.session || {})
          color: root.agentColor(row.session || {})
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.bold: String(row.session ? row.session.agent_state : "") === "working"
        }
      }

      Text {
        visible: row.activity !== ""
        width: parent.width
        text: row.activity
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        elide: Text.ElideRight
        maximumLineCount: 1
      }

      Text {
        width: parent.width
        text: root.factsLine(row.session || {})
        color: root.dim
        opacity: 0.85
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
        maximumLineCount: 1
      }
    }
  }
}
