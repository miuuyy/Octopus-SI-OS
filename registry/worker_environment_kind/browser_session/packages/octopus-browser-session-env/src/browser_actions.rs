use super::*;

pub(super) const DOM_HELPERS: &str = r###"
function $(e) {
  if (e.parentElement) return e.parentElement;
  if (e.parentNode && e.parentNode.nodeType === 11 && e.parentNode.host) return e.parentNode.host;
}
function Ar(e) {
  let t = e;
  for (; t.parentNode;) t = t.parentNode;
  if (t.nodeType === 11 || t.nodeType === 9) return t;
}
function kr(e) {
  for (; e.parentElement;) e = e.parentElement;
  return $(e);
}
function ye(e, t, r) {
  for (; e;) {
    const n = e.closest(t);
    if (r && n !== r && n?.contains(r)) return;
    if (n) return n;
    e = kr(e);
  }
}
function X(e, t) {
  const r = t === "::before" ? Se : t === "::after" ? _e : Ee;
  if (r && r.has(e)) return r.get(e);
  const n =
    e.ownerDocument && e.ownerDocument.defaultView
      ? e.ownerDocument.defaultView.getComputedStyle(e, t)
      : void 0;
  return (r?.set(e, n), n);
}
function Cr(e, t) {
  if (((t = t ?? X(e)), !t)) return !0;
  if (Element.prototype.checkVisibility && true) {
    if (!e.checkVisibility()) return !1;
  } else {
    const r = e.closest("details,summary");
    if (r !== e && r?.nodeName === "DETAILS" && !r.open) return !1;
  }
  return t.visibility === "visible";
}
function Ze(e) {
  const t = X(e);
  if (!t) return { visible: !0, inline: !1 };
  const r = t.cursor;
  if (t.display === "contents") {
    for (let i = e.firstChild; i; i = i.nextSibling) {
      if (i.nodeType === 1 && Q(i)) return { visible: !0, inline: !1, cursor: r };
      if (i.nodeType === 3 && Nr(i)) return { visible: !0, inline: !0, cursor: r };
    }
    return { visible: !1, inline: !1, cursor: r };
  }
  if (!Cr(e, t)) return { cursor: r, visible: !1, inline: !1 };
  const n = e.getBoundingClientRect();
  return { cursor: r, visible: n.width > 0 && n.height > 0, inline: t.display === "inline" };
}
function Q(e) {
  return Ze(e).visible;
}
function Nr(e) {
  const t = e.ownerDocument.createRange();
  t.selectNode(e);
  const r = t.getBoundingClientRect();
  return r.width > 0 && r.height > 0;
}
function _(e) {
  const t = e.tagName;
  return typeof t == "string"
    ? t.toUpperCase()
    : e instanceof HTMLFormElement
      ? "FORM"
      : e.tagName.toUpperCase();
}
var Ee,
  Se,
  _e,
  Ir = 0;
function _t() {
  (++Ir, Ee ?? (Ee = new Map()), Se ?? (Se = new Map()), _e ?? (_e = new Map()));
}
function Tt() {
  --Ir || ((Ee = void 0), (Se = void 0), (_e = void 0));
}
function $r(e) {
  return e.hasAttribute("aria-label") || e.hasAttribute("aria-labelledby");
}
var Lr =
    "article:not([role]), aside:not([role]), main:not([role]), nav:not([role]), section:not([role]), [role=article], [role=complementary], [role=main], [role=navigation], [role=region]",
  yi = [
    ["aria-atomic", void 0],
    ["aria-busy", void 0],
    ["aria-controls", void 0],
    ["aria-current", void 0],
    ["aria-describedby", void 0],
    ["aria-details", void 0],
    ["aria-dropeffect", void 0],
    ["aria-flowto", void 0],
    ["aria-grabbed", void 0],
    ["aria-hidden", void 0],
    ["aria-keyshortcuts", void 0],
    [
      "aria-label",
      [
        "caption",
        "code",
        "deletion",
        "emphasis",
        "generic",
        "insertion",
        "paragraph",
        "presentation",
        "strong",
        "subscript",
        "superscript",
      ],
    ],
    [
      "aria-labelledby",
      [
        "caption",
        "code",
        "deletion",
        "emphasis",
        "generic",
        "insertion",
        "paragraph",
        "presentation",
        "strong",
        "subscript",
        "superscript",
      ],
    ],
    ["aria-live", void 0],
    ["aria-owns", void 0],
    ["aria-relevant", void 0],
    ["aria-roledescription", ["generic"]],
  ];
function Rr(e, t) {
  return yi.some(([r, n]) => !n?.includes(t || "") && e.hasAttribute(r));
}
function Or(e) {
  return !Number.isNaN(Number(String(e.getAttribute("tabindex"))));
}
function Ei(e) {
  return !Kr(e) && (Si(e) || Or(e));
}
function Si(e) {
  const t = _(e);
  return ["BUTTON", "DETAILS", "SELECT", "TEXTAREA"].includes(t)
    ? !0
    : t === "A" || t === "AREA"
      ? e.hasAttribute("href")
      : t === "INPUT"
        ? !e.hidden
        : !1;
}
var Pr = {
  A: (e) => (e.hasAttribute("href") ? "link" : null),
  AREA: (e) => (e.hasAttribute("href") ? "link" : null),
  ARTICLE: () => "article",
  ASIDE: () => "complementary",
  BLOCKQUOTE: () => "blockquote",
  BUTTON: () => "button",
  CAPTION: () => "caption",
  CODE: () => "code",
  DATALIST: () => "listbox",
  DD: () => "definition",
  DEL: () => "deletion",
  DETAILS: () => "group",
  DFN: () => "term",
  DIALOG: () => "dialog",
  DT: () => "term",
  EM: () => "emphasis",
  FIELDSET: () => "group",
  FIGURE: () => "figure",
  FOOTER: (e) => (ye(e, Lr) ? null : "contentinfo"),
  FORM: (e) => ($r(e) ? "form" : null),
  H1: () => "heading",
  H2: () => "heading",
  H3: () => "heading",
  H4: () => "heading",
  H5: () => "heading",
  H6: () => "heading",
  HEADER: (e) => (ye(e, Lr) ? null : "banner"),
  HR: () => "separator",
  HTML: () => "document",
  IMG: (e) =>
    e.getAttribute("alt") === "" && !e.getAttribute("title") && !Rr(e) && !Or(e)
      ? "presentation"
      : "img",
  INPUT: (e) => {
    const t = e.type.toLowerCase();
    if (t === "search") return e.hasAttribute("list") ? "combobox" : "searchbox";
    if (["email", "tel", "text", "url", ""].includes(t)) {
      const r = le(e, e.getAttribute("list"))[0];
      return r && _(r) === "DATALIST" ? "combobox" : "textbox";
    }
    return t === "hidden" ? null : t === "file" ? "button" : Bi[t] || "textbox";
  },
  INS: () => "insertion",
  LI: () => "listitem",
  MAIN: () => "main",
  MARK: () => "mark",
  MATH: () => "math",
  MENU: () => "list",
  METER: () => "meter",
  NAV: () => "navigation",
  OL: () => "list",
  OPTGROUP: () => "group",
  OPTION: () => "option",
  OUTPUT: () => "status",
  P: () => "paragraph",
  PROGRESS: () => "progressbar",
  SEARCH: () => "search",
  SECTION: (e) => ($r(e) ? "region" : null),
  SELECT: (e) => (e.hasAttribute("multiple") || e.size > 1 ? "listbox" : "combobox"),
  STRONG: () => "strong",
  SUB: () => "subscript",
  SUP: () => "superscript",
  SVG: () => "img",
  TABLE: () => "table",
  TBODY: () => "rowgroup",
  TD: (e) => {
    const t = ye(e, "table"),
      r = t ? At(t) : "";
    return r === "grid" || r === "treegrid" ? "gridcell" : "cell";
  },
  TEXTAREA: () => "textbox",
  TFOOT: () => "rowgroup",
  TH: (e) => {
    const t = e.getAttribute("scope");
    if (t === "col" || t === "colgroup") return "columnheader";
    if (t === "row" || t === "rowgroup") return "rowheader";
    const r = e.nextElementSibling,
      n = e.previousElementSibling,
      i = e.parentElement && _(e.parentElement) === "TR" ? e.parentElement : void 0;
    if (!r && !n) {
      if (i) {
        const s = ye(i, "table");
        if (s && s.rows.length <= 1) return null;
      }
      return "columnheader";
    }
    return Mr(r) && Mr(n) ? "columnheader" : qr(r) || qr(n) ? "rowheader" : "columnheader";
  },
  THEAD: () => "rowgroup",
  TIME: () => "time",
  TR: () => "row",
  UL: () => "list",
};
function Mr(e) {
  return !!e && _(e) === "TH";
}
function qr(e) {
  var t;
  return !e || _(e) !== "TD"
    ? !1
    : !!(((t = e.textContent) != null && t.trim()) || e.children.length > 0);
}
var _i = {
  DD: ["DL", "DIV"],
  DIV: ["DL"],
  DT: ["DL", "DIV"],
  LI: ["OL", "UL"],
  TBODY: ["TABLE"],
  TD: ["TR"],
  TFOOT: ["TABLE"],
  TH: ["TR"],
  THEAD: ["TABLE"],
  TR: ["THEAD", "TBODY", "TFOOT", "TABLE"],
};
function Dr(e) {
  var t;
  const r = ((t = Pr[_(e)]) == null ? void 0 : t.call(Pr, e)) || "";
  if (!r) return null;
  let n = e;
  for (; n;) {
    const i = $(n),
      s = _i[_(n)];
    if (!s || !i || !s.includes(_(i))) break;
    const o = At(i);
    if ((o === "none" || o === "presentation") && !Br(i, o)) return o;
    n = i;
  }
  return r;
}
var Ti = [
  "alert",
  "alertdialog",
  "application",
  "article",
  "banner",
  "blockquote",
  "button",
  "caption",
  "cell",
  "checkbox",
  "code",
  "columnheader",
  "combobox",
  "complementary",
  "contentinfo",
  "definition",
  "deletion",
  "dialog",
  "directory",
  "document",
  "emphasis",
  "feed",
  "figure",
  "form",
  "generic",
  "grid",
  "gridcell",
  "group",
  "heading",
  "img",
  "insertion",
  "link",
  "list",
  "listbox",
  "listitem",
  "log",
  "main",
  "mark",
  "marquee",
  "math",
  "meter",
  "menu",
  "menubar",
  "menuitem",
  "menuitemcheckbox",
  "menuitemradio",
  "navigation",
  "none",
  "note",
  "option",
  "paragraph",
  "presentation",
  "progressbar",
  "radio",
  "radiogroup",
  "region",
  "row",
  "rowgroup",
  "rowheader",
  "scrollbar",
  "search",
  "searchbox",
  "separator",
  "slider",
  "spinbutton",
  "status",
  "strong",
  "subscript",
  "superscript",
  "switch",
  "tab",
  "table",
  "tablist",
  "tabpanel",
  "term",
  "textbox",
  "time",
  "timer",
  "toolbar",
  "tooltip",
  "tree",
  "treegrid",
  "treeitem",
];
function At(e) {
  return (
    (e.getAttribute("role") || "")
      .split(" ")
      .map((r) => r.trim())
      .find((r) => Ti.includes(r)) || null
  );
}
function Br(e, t) {
  return Rr(e, t) || Ei(e);
}
function I(e) {
  const t = At(e);
  if (!t) return Dr(e);
  if (t === "none" || t === "presentation") {
    const r = Dr(e);
    if (Br(e, r)) return r;
  }
  return t;
}
function le(e, t) {
  if (!t) return [];
  const r = Ar(e);
  if (!r) return [];
  try {
    const n = t.split(" ").filter((s) => !!s),
      i = [];
    for (const s of n) {
      const o = r.querySelector("#" + CSS.escape(s));
      o && !i.includes(o) && i.push(o);
    }
    return i;
  } catch {
    return [];
  }
}
var Bi = {
    button: "button",
    checkbox: "checkbox",
    image: "button",
    number: "spinbutton",
    radio: "radio",
    range: "slider",
    reset: "button",
    submit: "button",
  },
  Hi = 0;
var Oi = [
  "checkbox",
  "combobox",
  "grid",
  "gridcell",
  "listbox",
  "radiogroup",
  "slider",
  "spinbutton",
  "textbox",
  "columnheader",
  "rowheader",
  "searchbox",
  "switch",
  "treegrid",
];
function Pi(e) {
  const t = _(e);
  return ["INPUT", "TEXTAREA", "SELECT"].includes(t)
    ? e.hasAttribute("readonly")
    : Oi.includes(I(e) || "")
      ? e.getAttribute("aria-readonly") === "true"
      : e.isContentEditable
        ? !1
        : "error";
}
var Yr = [
  "application",
  "button",
  "composite",
  "gridcell",
  "group",
  "input",
  "link",
  "menuitem",
  "scrollbar",
  "separator",
  "tab",
  "checkbox",
  "columnheader",
  "combobox",
  "grid",
  "listbox",
  "menu",
  "menubar",
  "menuitemcheckbox",
  "menuitemradio",
  "option",
  "radio",
  "radiogroup",
  "row",
  "rowheader",
  "searchbox",
  "select",
  "slider",
  "spinbutton",
  "switch",
  "tablist",
  "textbox",
  "toolbar",
  "tree",
  "treegrid",
  "treeitem",
];
function et(e) {
  return Kr(e) || Zr(e);
}
function Kr(e) {
  return (
    ["BUTTON", "INPUT", "SELECT", "TEXTAREA", "OPTION", "OPTGROUP"].includes(_(e)) &&
    (e.hasAttribute("disabled") || Mi(e) || qi(e))
  );
}
function Mi(e) {
  return _(e) === "OPTION" && !!e.closest("OPTGROUP[DISABLED]");
}
function qi(e) {
  const t = e?.closest("FIELDSET[DISABLED]");
  if (!t) return !1;
  const r = t.querySelector(":scope > LEGEND");
  return !r || !r.contains(e);
}
function Zr(e, t = !1) {
  if (!e) return !1;
  if (t || Yr.includes(I(e) || "")) {
    const r = (e.getAttribute("aria-disabled") || "").toLowerCase();
    return r === "true" ? !0 : r === "false" ? !1 : Zr($(e), !0);
  }
  return !1;
}
"###;

const MAX_TRACKED_SESSIONS: usize = 256;
const MAX_OVERFLOW_SESSIONS: usize = 4096;
const MAX_FRAME_TREE_ENTRIES: usize = 2048;
const MAX_SNAPSHOT_FRAMES: usize = 64;
const MAX_FRAME_REVISIONS: usize = 2048;
const MAX_LOADING_FRAMES: usize = 512;
const CHILD_FRAME_CAPTURE_BUDGET: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FrameRoute {
    pub session_id: Option<String>,
    pub attachment_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachedSession {
    session_id: String,
    parent_session_id: Option<String>,
    target_id: String,
    attachment_seq: u64,
}

#[derive(Default)]
pub(super) struct FrameState {
    sessions: BTreeMap<String, AttachedSession>,
    overflowed_sessions: BTreeSet<String>,
    sequence: u64,
    revisions: BTreeMap<(Option<String>, String), u64>,
    detached_revisions: BTreeSet<(Option<String>, String)>,
    contexts: BTreeMap<Option<String>, u64>,
    navigation_revision: u64,
    loading: BTreeSet<(Option<String>, String)>,
}

impl FrameState {
    pub fn route(&self, id: Option<&str>) -> Result<FrameRoute, String> {
        let mut current = id;
        let mut seen = BTreeSet::new();
        while let Some(id) = current {
            if !seen.insert(id) || seen.len() > MAX_TRACKED_SESSIONS {
                return Err("browser iframe ancestry is cyclic or excessive".into());
            }
            current = self
                .sessions
                .get(id)
                .ok_or("browser iframe attachment is unavailable")?
                .parent_session_id
                .as_deref();
        }
        Ok(FrameRoute {
            session_id: id.map(str::to_string),
            attachment_seq: id.map(|id| self.sessions[id].attachment_seq).unwrap_or(0),
        })
    }

    fn is_ancestor_route(&self, ancestor: &FrameRoute, descendant: &FrameRoute) -> bool {
        if ancestor == descendant
            || self.route(ancestor.session_id.as_deref()).ok().as_ref() != Some(ancestor)
            || self.route(descendant.session_id.as_deref()).ok().as_ref() != Some(descendant)
        {
            return false;
        }
        let mut current = descendant.session_id.as_deref();
        let mut steps = 0usize;
        while let Some(id) = current {
            if steps >= MAX_TRACKED_SESSIONS {
                return false;
            }
            steps += 1;
            let Some(entry) = self.sessions.get(id) else {
                return false;
            };
            if entry.parent_session_id == ancestor.session_id {
                return true;
            }
            current = entry.parent_session_id.as_deref();
        }
        false
    }

    pub fn inventory(&mut self, target: &str, value: &Value) -> Result<(), String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Inventory {
            target_id: String,
            sessions: Vec<AttachedSession>,
        }
        if serde_json::to_vec(value)
            .map_err(|error| error.to_string())?
            .len()
            > 65536
        {
            return Err("native frame inventory exceeds its byte limit".into());
        }
        if !value["sessions"].as_array().is_some_and(|rows| {
            rows.iter()
                .all(|row| row.get("parent_session_id").is_some())
        }) {
            return Err("native frame inventory omitted required parent session field".into());
        }
        let inventory: Inventory = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid native frame inventory: {error}"))?;
        if inventory.target_id != target || inventory.sessions.len() > MAX_TRACKED_SESSIONS {
            return Err("native frame inventory is outside this root target".into());
        }
        let mut next = BTreeMap::new();
        let mut sequences = BTreeSet::new();
        let mut maximum = self.sequence;
        for entry in inventory.sessions {
            if entry.session_id.is_empty()
                || entry.target_id.is_empty()
                || entry.attachment_seq == 0
                || entry.attachment_seq > 9_007_199_254_740_991
                || !sequences.insert(entry.attachment_seq)
                || next.contains_key(&entry.session_id)
                || entry
                    .parent_session_id
                    .as_ref()
                    .is_some_and(|id| !next.contains_key(id))
            {
                return Err("native frame inventory is not finite, unique and parent-first".into());
            }
            if self.sessions.get(&entry.session_id) != Some(&entry)
                && entry.attachment_seq <= self.sequence
            {
                return Err("native iframe attachment sequence was reused".into());
            }
            maximum = maximum.max(entry.attachment_seq);
            next.insert(entry.session_id.clone(), entry);
        }
        self.sessions = next;
        self.overflowed_sessions.clear();
        self.sequence = maximum;
        Ok(())
    }

    pub fn record(&mut self, event: &Value, native: bool) -> Result<(), String> {
        let route = event.get("sessionId").and_then(Value::as_str);
        let params = &event["params"];
        if route.is_some_and(|id| self.overflowed_sessions.contains(id)) {
            match event["method"].as_str() {
                Some("Target.attachedToTarget")
                    if params["targetInfo"]["type"] == "iframe" && !native =>
                {
                    if let Some(id) = params["sessionId"].as_str().filter(|id| !id.is_empty()) {
                        if self.overflowed_sessions.len() < MAX_OVERFLOW_SESSIONS {
                            self.overflowed_sessions.insert(id.into());
                        }
                    }
                }
                Some("Target.detachedFromTarget") => {
                    if let Some(id) = params["sessionId"].as_str() {
                        self.overflowed_sessions.remove(id);
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        match event["method"].as_str() {
            Some("Target.attachedToTarget")
                if params["targetInfo"]["type"] == "iframe" && !native =>
            {
                self.route(route)?;
                let id = params["sessionId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("iframe attachment has no session id")?;
                let target = params["targetInfo"]["targetId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("iframe attachment has no target id")?;
                if let Some(existing) = self.sessions.get(id) {
                    if existing.target_id != target
                        || existing.parent_session_id.as_deref() != route
                    {
                        return Err("iframe attachment identity changed".into());
                    }
                } else {
                    if route == Some(id) {
                        return Err("iframe attachment exceeds its ancestry boundary".into());
                    }
                    if self.sessions.len() >= MAX_TRACKED_SESSIONS {
                        if self.overflowed_sessions.len() < MAX_OVERFLOW_SESSIONS {
                            self.overflowed_sessions.insert(id.into());
                        }
                        return Ok(());
                    }
                    self.sequence = self
                        .sequence
                        .checked_add(1)
                        .ok_or("iframe attachment sequence exhausted")?;
                    self.sessions.insert(
                        id.into(),
                        AttachedSession {
                            session_id: id.into(),
                            parent_session_id: route.map(str::to_string),
                            target_id: target.into(),
                            attachment_seq: self.sequence,
                        },
                    );
                }
            }
            Some("Page.frameStartedLoading") => {
                if let Some(frame) = params["frameId"].as_str() {
                    self.navigation_revision = self.navigation_revision.saturating_add(1);
                    self.loading
                        .insert((route.map(str::to_string), frame.into()));
                    if self.loading.len() > MAX_LOADING_FRAMES {
                        return Err("loading frame inventory exceeds its boundary".into());
                    }
                }
            }
            Some("Page.frameStoppedLoading" | "Page.lifecycleEvent") => {
                if event["method"] == "Page.frameStoppedLoading"
                    || matches!(params["name"].as_str(), Some("DOMContentLoaded" | "load"))
                {
                    if let Some(frame) = params["frameId"].as_str() {
                        self.loading
                            .remove(&(route.map(str::to_string), frame.into()));
                    }
                }
            }
            Some("Target.detachedFromTarget") => {
                if let Some(id) = params["sessionId"].as_str() {
                    if self.overflowed_sessions.remove(id) {
                        return Ok(());
                    }
                }
                let mut removed = BTreeSet::new();
                if let Some(id) = params["sessionId"].as_str() {
                    removed.insert(id.to_string());
                }
                loop {
                    let before = removed.len();
                    for entry in self.sessions.values() {
                        if entry
                            .parent_session_id
                            .as_ref()
                            .is_some_and(|id| removed.contains(id))
                        {
                            removed.insert(entry.session_id.clone());
                        }
                    }
                    if before == removed.len() {
                        break;
                    }
                }
                self.loading
                    .retain(|(id, _)| !id.as_ref().is_some_and(|id| removed.contains(id)));
                self.sessions.retain(|id, _| !removed.contains(id));
                self.revisions
                    .retain(|(id, _), _| !id.as_ref().is_some_and(|id| removed.contains(id)));
                self.detached_revisions
                    .retain(|(id, _)| !id.as_ref().is_some_and(|id| removed.contains(id)));
                self.contexts
                    .retain(|id, _| !id.as_ref().is_some_and(|id| removed.contains(id)));
            }
            Some("Page.frameNavigated" | "Page.frameDetached" | "Page.navigatedWithinDocument") => {
                if event["method"] == "Page.navigatedWithinDocument"
                    && params["navigationType"] == "fragment"
                {
                    return Ok(());
                }
                self.navigation_revision = self.navigation_revision.saturating_add(1);
                if event["method"] == "Page.frameNavigated"
                    && route.is_none()
                    && params["frame"].get("parentId").is_none()
                {
                    if let Some(main_id) = params["frame"]["id"].as_str() {
                        self.loading
                            .retain(|(r, fid)| r.is_some() || fid == main_id);
                        self.revisions
                            .retain(|(r, fid), _| r.is_some() || fid == main_id);
                        self.detached_revisions
                            .retain(|(r, fid)| r.is_some() || fid == main_id);
                    }
                }
                if event["method"] == "Page.frameDetached" {
                    if let Some(frame) = params["frameId"].as_str() {
                        let key = (route.map(str::to_string), frame.into());
                        self.loading.remove(&key);
                        self.detached_revisions.insert(key);
                    }
                }
                let id = params["frame"]["id"]
                    .as_str()
                    .or_else(|| params["frameId"].as_str());
                if let Some(id) = id {
                    let key = (route.map(str::to_string), id.into());
                    if event["method"] != "Page.frameDetached" {
                        self.detached_revisions.remove(&key);
                    }
                    let revision = self.revisions.entry(key).or_default();
                    *revision = revision.saturating_add(1);
                }
                if self.revisions.len() > MAX_FRAME_REVISIONS {
                    for key in std::mem::take(&mut self.detached_revisions) {
                        self.revisions.remove(&key);
                    }
                }
                if self.revisions.len() > MAX_FRAME_REVISIONS {
                    return Err("browser frame revision inventory exceeds its bound; reconnect for a fresh snapshot".into());
                }
            }
            Some("DOM.documentUpdated" | "Runtime.executionContextsCleared") => {
                let revision = self.contexts.entry(route.map(str::to_string)).or_default();
                *revision = revision.saturating_add(1);
            }
            _ => {}
        }
        Ok(())
    }
    fn revision(&self, route: &FrameRoute, frame: &str) -> (u64, u64) {
        (
            *self
                .revisions
                .get(&(route.session_id.clone(), frame.into()))
                .unwrap_or(&0),
            *self.contexts.get(&route.session_id).unwrap_or(&0),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FrameDocument {
    pub document: DocumentIdentity,
    pub route: FrameRoute,
    revision: u64,
    context_revision: u64,
    parent_id: Option<String>,
    owner_backend_node_id: Option<i64>,
}

fn tree_documents(tree: &Value) -> Result<BTreeMap<String, Value>, String> {
    let mut queue = VecDeque::from([tree]);
    let mut documents = BTreeMap::new();
    while let Some(node) = queue.pop_front() {
        let id = node["frame"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("browser frame tree has no frame identity")?;
        if documents.insert(id.into(), node["frame"].clone()).is_some() {
            return Err("browser frame tree is cyclic".into());
        }
        if documents.len() > MAX_FRAME_TREE_ENTRIES {
            return Err("browser frame tree exceeds safety limit".into());
        }
        if let Some(children) = node.get("childFrames") {
            queue.extend(
                children
                    .as_array()
                    .ok_or("browser child frame list is malformed")?,
            );
        }
    }
    Ok(documents)
}

async fn validate_frame_in_trees(
    session: &mut CdpSession,
    trees: &mut BTreeMap<Option<String>, BTreeMap<String, Value>>,
    frame: &FrameDocument,
) -> Result<(), String> {
    let id = frame.route.session_id.as_deref();
    if session.frames.route(id)? != frame.route {
        return Err("browser ref belongs to a detached iframe; call browser.snapshot".into());
    }
    if !trees.contains_key(&frame.route.session_id) {
        let tree = session
            .command_in("browser.observe", id, "Page.getFrameTree", json!({}))
            .await?;
        trees.insert(
            frame.route.session_id.clone(),
            tree_documents(&tree["frameTree"])?,
        );
    }
    let current = trees[&frame.route.session_id]
        .get(&frame.document.frame_id)
        .ok_or("browser ref frame is no longer in its owning target")?;
    if document_identity(current)? != frame.document
        || session
            .frames
            .revision(&frame.route, &frame.document.frame_id)
            != (frame.revision, frame.context_revision)
    {
        return Err(
            "browser ref belongs to a stale document in its frame; call browser.snapshot".into(),
        );
    }
    Ok(())
}

pub(super) async fn validate_frames(
    session: &mut CdpSession,
    path: &[FrameDocument],
) -> Result<(), String> {
    let mut trees = BTreeMap::new();
    for frame in path {
        validate_frame_in_trees(session, &mut trees, frame).await?;
    }
    for frame in path {
        if session.frames.route(frame.route.session_id.as_deref())? != frame.route
            || session
                .frames
                .revision(&frame.route, &frame.document.frame_id)
                != (frame.revision, frame.context_revision)
        {
            return Err("browser frame changed while its provenance was validated".into());
        }
    }
    Ok(())
}

pub(super) struct FrameCapture {
    pub nodes: Vec<Value>,
    pub provenance: BTreeMap<String, Vec<FrameDocument>>,
    pub warnings: Vec<String>,
    pub documents: Vec<FrameDocument>,
}

fn snapshot_string<'a>(snapshot: &'a Value, index: &Value) -> Result<&'a str, String> {
    index
        .as_u64()
        .and_then(|index| snapshot["strings"].get(index as usize))
        .and_then(Value::as_str)
        .ok_or_else(|| "DOM snapshot string reference is malformed".into())
}

pub(super) async fn capture_frames(session: &mut CdpSession) -> Result<FrameCapture, String> {
    let mut queue = VecDeque::from([session.frames.route(None)?]);
    let mut captures = BTreeMap::new();
    let mut capture_order = Vec::new();
    let mut capture_bytes = 0usize;
    let mut attempted = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut omitted_sessions = 0usize;
    let mut child_budget_exceeded = false;
    let child_capture_start = Instant::now();
    while let Some(route) = queue.pop_front() {
        if !attempted.insert(route.session_id.clone()) {
            continue;
        }
        let id = route.session_id.as_deref();
        if id.is_some() {
            if session.frames.route(id).ok().as_ref() != Some(&route) {
                continue;
            }
            if captures.len() >= MAX_SNAPSHOT_FRAMES {
                omitted_sessions += 1;
                continue;
            }
            if child_capture_start.elapsed() >= CHILD_FRAME_CAPTURE_BUDGET {
                child_budget_exceeded = true;
                omitted_sessions += 1;
                continue;
            }
        }
        let capture = async {
            for method in ["Page.enable", "Runtime.enable", "Accessibility.enable"] {
                session.command_in("browser.snapshot", id, method, json!({})).await?;
            }
            session.command_in("browser.snapshot", id, "Target.setAutoAttach", json!({"autoAttach":true,"flatten":true,"waitForDebuggerOnStart":false,"filter":[{"type":"iframe","exclude":false}]})).await?;
            session.command_in("browser.snapshot", id, "Page.setLifecycleEventsEnabled", json!({"enabled":true})).await?;
            let tree = session.command_in("browser.snapshot", id, "Page.getFrameTree", json!({})).await?;
            let snapshot = session.command_in("browser.snapshot", id, "DOMSnapshot.captureSnapshot", json!({"computedStyles":[],"includePaintOrder":false,"includeDOMRects":false})).await?;
            Ok::<_,String>((tree_documents(&tree["frameTree"])?, snapshot))
        }.await;
        match capture {
            Ok(capture) => {
                capture_bytes = capture_bytes.saturating_add(
                    serde_json::to_vec(&capture.1)
                        .map_err(|error| error.to_string())?
                        .len(),
                );
                if capture_bytes > 96 * 1024 * 1024 {
                    return Err("frame DOM capture exceeds its aggregate byte limit".into());
                }
                capture_order.push(route.session_id.clone());
                captures.insert(route.session_id.clone(), (route.clone(), capture));
            }
            Err(error) if id.is_some() && session.usable => {
                warnings.push(format!("Iframe accessibility unavailable: {error}"))
            }
            Err(error) => return Err(error),
        }
        for entry in session.frames.sessions.values() {
            if !attempted.contains(&Some(entry.session_id.clone())) {
                queue.push_back(session.frames.route(Some(&entry.session_id))?);
            }
        }
    }
    if !session.frames.overflowed_sessions.is_empty() {
        warnings.push(format!(
            "Skipped {} additional iframe attachment(s) after reaching session tracking limit ({MAX_TRACKED_SESSIONS})",
            session.frames.overflowed_sessions.len()
        ));
    }
    if omitted_sessions > 0 {
        if child_budget_exceeded {
            warnings.push(format!(
                "Skipped {omitted_sessions} additional iframe session(s) after reaching child frame capture time budget"
            ));
        } else {
            warnings.push(format!(
                "Skipped {omitted_sessions} additional iframe session(s) after reaching snapshot frame limit ({MAX_SNAPSHOT_FRAMES})"
            ));
        }
    }
    let root = current_document(session).await?.frame_id;
    let mut live_capture_order = Vec::new();
    for key in capture_order {
        let route = &captures[&key].0;
        if route.session_id.is_some()
            && session.frames.route(route.session_id.as_deref()).ok().as_ref() != Some(route)
        {
            warnings.push("Iframe session detached during capture".into());
            continue;
        }
        live_capture_order.push(key);
    }
    let mut owners: BTreeMap<String, (Option<String>, usize)> = BTreeMap::new();
    for key in &live_capture_order {
        let (route, (tree, snapshot)) = &captures[key];
        let rows = snapshot["documents"]
            .as_array()
            .ok_or("DOM snapshot has no documents")?;
        let mut seen_in_capture = BTreeSet::new();
        for (row_index, row) in rows.iter().enumerate() {
            let frame_id = snapshot_string(snapshot, &row["frameId"])?;
            if !seen_in_capture.insert(frame_id) {
                return Err("DOM snapshot frame ownership is ambiguous".into());
            }
            if !tree.contains_key(frame_id) {
                if frame_id == root || (route.session_id.is_none() && row_index == 0) {
                    return Err("DOM snapshot frame is absent from its owning frame tree".into());
                }
                warnings.push(format!(
                    "Iframe {frame_id} accessibility unavailable: frame absent from owning frame tree"
                ));
                continue;
            }
            match owners.get(frame_id) {
                None => {
                    owners.insert(frame_id.to_string(), (key.clone(), row_index));
                }
                Some((prev_key, _)) => {
                    let prev_route = &captures[prev_key].0;
                    if session.frames.is_ancestor_route(prev_route, route) {
                        owners.insert(frame_id.to_string(), (key.clone(), row_index));
                    } else if !session.frames.is_ancestor_route(route, prev_route) {
                        return Err("DOM snapshot frame ownership is ambiguous".into());
                    }
                }
            }
        }
    }
    let mut selected_order = Vec::new();
    let mut selected_set = BTreeSet::new();
    let mut omitted_frame_ids = BTreeSet::new();
    if owners.contains_key(&root) {
        selected_order.push(root.clone());
        selected_set.insert(root.clone());
    }
    for key in &live_capture_order {
        let (_, (_, snapshot)) = &captures[key];
        let Some(rows) = snapshot["documents"].as_array() else {
            continue;
        };
        for (row_index, row) in rows.iter().enumerate() {
            let frame_id = snapshot_string(snapshot, &row["frameId"])?;
            if owners.get(frame_id) != Some(&(key.clone(), row_index))
                || selected_set.contains(frame_id)
            {
                continue;
            }
            if selected_order.len() < MAX_SNAPSHOT_FRAMES {
                selected_order.push(frame_id.to_string());
                selected_set.insert(frame_id.to_string());
            } else {
                omitted_frame_ids.insert(frame_id.to_string());
            }
        }
    }
    if !omitted_frame_ids.is_empty() {
        warnings.push(format!(
            "Omitted {} additional iframe document(s) after reaching snapshot frame limit ({MAX_SNAPSHOT_FRAMES})",
            omitted_frame_ids.len()
        ));
    }
    let mut documents = BTreeMap::new();
    let mut edges = BTreeMap::new();
    let mut described_iframes = 0usize;
    let mut omitted_iframe_elements = 0usize;
    for frame_id in &selected_order {
        let (key, row_index) = &owners[frame_id];
        let (route, (tree, snapshot)) = &captures[key];
        let row = &snapshot["documents"][*row_index];
        let frame = &tree[frame_id];
        let document = document_identity(frame)?;
        let (revision, context_revision) = session.frames.revision(route, frame_id);
        documents.insert(
            frame_id.clone(),
            FrameDocument {
                document,
                route: route.clone(),
                revision,
                context_revision,
                parent_id: None,
                owner_backend_node_id: None,
            },
        );
        let names = row["nodes"]["nodeName"]
            .as_array()
            .ok_or("DOM snapshot has no node names")?;
        let backend_ids = row["nodes"]["backendNodeId"]
            .as_array()
            .ok_or("DOM snapshot has no node identities")?;
        if names.len() != backend_ids.len() {
            return Err("DOM snapshot node columns differ".into());
        }
        for (index, name) in names.iter().enumerate() {
            if !matches!(snapshot_string(snapshot, name)?, "IFRAME" | "FRAME") {
                continue;
            }
            let owner = backend_ids[index]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("iframe owner has no backend node id")?;
            if edges.len() >= MAX_SNAPSHOT_FRAMES || described_iframes >= MAX_SNAPSHOT_FRAMES * 2 {
                omitted_iframe_elements += 1;
                continue;
            }
            described_iframes += 1;
            let child = match session
                .command_in(
                    "browser.snapshot",
                    route.session_id.as_deref(),
                    "DOM.describeNode",
                    json!({"backendNodeId":owner}),
                )
                .await
            {
                Ok(child) => child,
                Err(error) if session.usable => {
                    warnings.push(format!(
                        "Iframe in {frame_id} accessibility unavailable: {error}"
                    ));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let Some(child) = child["node"]["frameId"].as_str() else {
                warnings.push(format!(
                    "Iframe in {frame_id} accessibility unavailable: no child frame identity"
                ));
                continue;
            };
            if edges
                .insert(
                    child.to_string(),
                    (frame_id.to_string(), owner, route.clone()),
                )
                .is_some()
            {
                return Err("iframe has multiple DOM owners".into());
            }
        }
    }
    if omitted_iframe_elements > 0 {
        warnings.push(format!(
            "Omitted {omitted_iframe_elements} additional iframe element(s) after reaching snapshot frame limit ({MAX_SNAPSHOT_FRAMES})"
        ));
    }
    for (id, (parent, owner, parent_route)) in &edges {
        let Some(child_route) = documents.get(id).map(|child| child.route.clone()) else {
            if !omitted_frame_ids.contains(id)
                && omitted_sessions == 0
                && session.frames.overflowed_sessions.is_empty()
            {
                warnings.push(format!(
                    "Iframe {id} accessibility unavailable: no attached document"
                ));
            }
            continue;
        };
        if let Some(declared_parent) = captures[&child_route.session_id].1 .0[id]
            .get("parentId")
            .and_then(Value::as_str)
        {
            if declared_parent != parent {
                return Err("iframe frame tree does not match its DOM parent".into());
            }
        }
        if child_route != *parent_route {
            let session_id = child_route
                .session_id
                .as_deref()
                .ok_or("child frame lost its owning session")?;
            let Some(entry) = session.frames.sessions.get(session_id) else {
                if !session.usable {
                    return Err("child frame was detached".into());
                }
                documents.remove(id);
                warnings.push(format!(
                    "Iframe {id} accessibility unavailable: child frame was detached"
                ));
                continue;
            };
            if entry.parent_session_id != parent_route.session_id {
                return Err("iframe debugger ancestry differs from its DOM owner".into());
            }
        }
        let actual = match session
            .command_in(
                "browser.snapshot",
                parent_route.session_id.as_deref(),
                "DOM.getFrameOwner",
                json!({"frameId":id}),
            )
            .await
        {
            Ok(actual) => actual,
            Err(error) if session.usable => {
                documents.remove(id);
                warnings.push(format!(
                    "Iframe {id} accessibility unavailable: {error}"
                ));
                continue;
            }
            Err(error) => return Err(error),
        };
        if actual["backendNodeId"].as_i64() != Some(*owner) {
            if !session.usable {
                return Err("iframe DOM owner changed during capture".into());
            }
            documents.remove(id);
            warnings.push(format!(
                "Iframe {id} accessibility unavailable: DOM owner changed during capture"
            ));
            continue;
        }
        if let Some(child) = documents.get_mut(id) {
            child.parent_id = Some(parent.clone());
            child.owner_backend_node_id = Some(*owner);
        }
    }
    let path_for = |frame: &FrameDocument| -> Result<Vec<FrameDocument>, String> {
        let mut path = vec![];
        let mut current = frame;
        loop {
            if path.len() >= documents.len() {
                return Err("iframe document ancestry is cyclic".into());
            }
            path.push(current.clone());
            match &current.parent_id {
                Some(parent) => {
                    current = documents
                        .get(parent)
                        .ok_or("iframe parent document is unavailable")?
                }
                None => break,
            }
        }
        if current.document.frame_id != root || current.route.session_id.is_some() {
            return Err("iframe document is not attached to the owned root".into());
        }
        path.reverse();
        Ok(path)
    };
    let mut ordered = Vec::new();
    for frame in documents.values() {
        match path_for(frame) {
            Ok(path) => ordered.push((frame.clone(), path)),
            Err(error) => warnings.push(format!(
                "Iframe {} accessibility unavailable: {error}",
                frame.document.frame_id
            )),
        }
    }
    ordered.sort_by_key(|(_, path)| path.len());
    let mut captured_ax = Vec::new();
    let mut ax_bytes = 0usize;
    for (frame, path) in ordered {
        let capture = session
            .command_in(
                "browser.snapshot",
                frame.route.session_id.as_deref(),
                "Accessibility.getFullAXTree",
                json!({"frameId":frame.document.frame_id}),
            )
            .await;
        let result = match capture {
            Ok(value) => value,
            Err(error) if frame.parent_id.is_some() && session.usable => {
                warnings.push(format!(
                    "Iframe {} accessibility unavailable: {error}",
                    frame.document.frame_id
                ));
                continue;
            }
            Err(error) => return Err(error),
        };
        let child_nodes = result["nodes"]
            .as_array()
            .ok_or("frame accessibility result has no nodes")?
            .clone();
        if child_nodes.is_empty() {
            if frame.parent_id.is_none() {
                return Err("root frame returned empty accessibility data".into());
            }
            warnings.push(format!(
                "Iframe {} accessibility unavailable: empty AX tree",
                frame.document.frame_id
            ));
            continue;
        }
        ax_bytes = ax_bytes.saturating_add(
            serde_json::to_vec(&child_nodes)
                .map_err(|error| error.to_string())?
                .len(),
        );
        if ax_bytes > 96 * 1024 * 1024 {
            return Err("frame AX capture exceeds its aggregate byte limit".into());
        }
        captured_ax.push((frame, path, child_nodes));
    }
    let mut trees = BTreeMap::new();
    let mut nodes = Vec::<Value>::new();
    let mut provenance = BTreeMap::new();
    let mut included = Vec::new();
    let mut included_ids = BTreeSet::new();
    for (frame, path, mut child_nodes) in captured_ax {
        if let Some(parent) = &frame.parent_id {
            if !included_ids.contains(parent) {
                warnings.push(format!(
                    "Iframe {} accessibility unavailable: parent iframe was excluded",
                    frame.document.frame_id
                ));
                continue;
            }
            if let Err(error) = validate_frame_in_trees(session, &mut trees, &frame).await {
                if !session.usable {
                    return Err(error);
                }
                warnings.push(format!(
                    "Iframe {} accessibility unavailable: {error}",
                    frame.document.frame_id
                ));
                continue;
            }
        }
        let namespace = |id: &str| json!([frame.document.frame_id, id]).to_string();
        for node in &mut child_nodes {
            let id = node["nodeId"].as_str().ok_or("AX node has no identity")?;
            let id = namespace(id);
            node["nodeId"] = json!(id);
            provenance.insert(id, path.clone());
            if let Some(ids) = node.get_mut("childIds").and_then(Value::as_array_mut) {
                for id in ids {
                    *id = json!(namespace(id.as_str().ok_or("AX child id is malformed")?));
                }
            }
            if let Some(id) = node.get_mut("parentId") {
                *id = json!(namespace(id.as_str().ok_or("AX parent id is malformed")?));
            }
        }
        if let Some(parent) = &frame.parent_id {
            let owner = nodes.iter().position(|node| {
                node["ignored"] != true
                    && node["backendDOMNodeId"].as_i64() == frame.owner_backend_node_id
                    && provenance
                        .get(node["nodeId"].as_str().unwrap_or(""))
                        .and_then(|path| path.last())
                        .is_some_and(|frame| &frame.document.frame_id == parent)
            });
            let Some(owner) = owner else {
                warnings.push(format!(
                    "Iframe {} accessibility unavailable: parent iframe AX owner missing",
                    frame.document.frame_id
                ));
                continue;
            };
            let ids: BTreeSet<String> = child_nodes
                .iter()
                .flat_map(|node| {
                    node["childIds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                })
                .collect();
            let mut seen_roots = BTreeSet::new();
            let roots: Vec<Value> = child_nodes
                .iter()
                .filter_map(|node| node["nodeId"].as_str())
                .filter(|id| !ids.contains(*id) && seen_roots.insert(*id))
                .map(|id| json!(id))
                .collect();
            if !nodes[owner]["childIds"].is_array() {
                nodes[owner]["childIds"] = json!([]);
            }
            nodes[owner]["childIds"]
                .as_array_mut()
                .ok_or("iframe AX child list is malformed")?
                .extend(roots);
        }
        included_ids.insert(frame.document.frame_id.clone());
        included.push(frame);
        nodes.extend(child_nodes);
    }
    if !included.iter().any(|frame| frame.document.frame_id == root) {
        return Err("root frame accessibility is unavailable".into());
    }
    Ok(FrameCapture {
        nodes,
        provenance,
        warnings,
        documents: included,
    })
}

async fn frame_object(
    session: &mut CdpSession,
    tool: &str,
    frame: &FrameDocument,
    backend: i64,
) -> Result<String, String> {
    let route = frame.route.session_id.as_deref();
    let world = session.command_in(tool,route,"Page.createIsolatedWorld",json!({"frameId":frame.document.frame_id,"worldName":"octopus.browser.actions","grantUniveralAccess":false})).await?;
    let context = world["executionContextId"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or("frame isolated world has no execution context")?;
    let node = session
        .command_in(
            tool,
            route,
            "DOM.resolveNode",
            json!({"backendNodeId":backend,"executionContextId":context}),
        )
        .await?;
    node["object"]["objectId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "frame node is not available in its exact execution context".into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentIdentity {
    pub frame_id: String,
    pub loader_id: String,
}

#[derive(Clone, Debug)]
pub(super) struct BrowserRef {
    pub backend_node_id: i64,
    pub target_id: String,
    pub frame_path: Vec<FrameDocument>,
}

#[derive(Default)]
pub(super) struct ObservationState {
    document: Option<DocumentIdentity>,
    pub document_revision: u64,
    navigation_revision: u64,
    loading: bool,
    committed: bool,
    painted: bool,
    last_ax_update: Option<Instant>,
    last_snapshot_navigation: u64,
}

#[derive(Clone)]
pub(super) struct NavigationStart {
    document: DocumentIdentity,
    revision: u64,
    frame_revision: u64,
}

pub(super) enum NavigationExpectation {
    Document(DocumentIdentity),
    Change(NavigationStart),
    Current,
}

impl ObservationState {
    pub fn ax_updated(&mut self) {
        self.last_ax_update = Some(Instant::now());
    }
    pub fn record(&mut self, event: &Value) {
        let params = &event["params"];
        let main_frame = self
            .document
            .as_ref()
            .map(|document| document.frame_id.as_str());
        match event["method"].as_str() {
            Some("Accessibility.nodesUpdated" | "Accessibility.loadComplete") => {
                self.last_ax_update = Some(Instant::now());
            }
            Some("Page.frameStartedLoading") if params["frameId"].as_str() == main_frame => {
                self.loading = true;
                self.committed = false;
                self.painted = false;
                self.navigation_revision = self.navigation_revision.saturating_add(1);
            }
            Some("Page.frameNavigated") if params["frame"].get("parentId").is_none() => {
                self.document_revision = self.document_revision.saturating_add(1);
                self.navigation_revision = self.navigation_revision.saturating_add(1);
                self.document = document_identity(&params["frame"]).ok();
                self.committed = true;
                self.painted = false;
            }
            Some("Page.navigatedWithinDocument") if params["frameId"].as_str() == main_frame => {
                self.navigation_revision = self.navigation_revision.saturating_add(1);
                if params["navigationType"] != "fragment" {
                    self.document_revision = self.document_revision.saturating_add(1);
                }
                self.loading = false;
                self.committed = true;
            }
            Some("Page.frameStoppedLoading") if params["frameId"].as_str() == main_frame => {
                self.loading = false;
            }
            Some("Page.lifecycleEvent")
                if params["frameId"].as_str() == main_frame
                    && self
                        .document
                        .as_ref()
                        .is_some_and(|document| params["loaderId"] == document.loader_id) =>
            {
                match params["name"].as_str() {
                    Some(
                        "firstPaint"
                        | "firstContentfulPaint"
                        | "firstMeaningfulPaint"
                        | "firstImagePaint",
                    ) => self.painted = true,
                    _ => {}
                }
                if matches!(params["name"].as_str(), Some("DOMContentLoaded" | "load")) {
                    self.loading = false;
                }
            }
            Some("DOM.documentUpdated" | "Runtime.executionContextsCleared") => {
                self.document_revision = self.document_revision.saturating_add(1);
            }
            _ => {}
        }
    }
}

fn document_identity(frame: &Value) -> Result<DocumentIdentity, String> {
    let field = |key| {
        frame
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("browser document has no {key}"))
    };
    Ok(DocumentIdentity {
        frame_id: field("id")?,
        loader_id: field("loaderId")?,
    })
}

pub(super) async fn current_document(session: &mut CdpSession) -> Result<DocumentIdentity, String> {
    let observed_revision = session.observation.document_revision;
    let frame_tree = session
        .command("browser.observe", "Page.getFrameTree", json!({}))
        .await?;
    let document = document_identity(&frame_tree["frameTree"]["frame"])?;
    if observed_revision != session.observation.document_revision
        && session.observation.document.as_ref() != Some(&document)
    {
        return Err("browser frame tree changed while the document was being read".into());
    }
    if session.observation.document.as_ref() != Some(&document) {
        session.observation.document_revision =
            session.observation.document_revision.saturating_add(1);
        session.observation.document = Some(document.clone());
        session.observation.committed = true;
        session.observation.painted = false;
    }
    Ok(document)
}

pub(super) async fn arm_navigation(session: &mut CdpSession) -> Result<NavigationStart, String> {
    session
        .command(
            "browser.observe",
            "Page.setLifecycleEventsEnabled",
            json!({"enabled":true}),
        )
        .await?;
    session
        .command("browser.observe", "Accessibility.enable", json!({}))
        .await?;
    let document = current_document(session).await?;
    Ok(NavigationStart {
        document,
        revision: session.observation.navigation_revision,
        frame_revision: session.frames.navigation_revision,
    })
}

pub(super) fn navigation_expectation(
    result: &Value,
    start: NavigationStart,
) -> Result<NavigationExpectation, String> {
    let frame_id = result
        .get("frameId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("browser navigation returned no frameId")?;
    match result.get("loaderId") {
        Some(Value::String(loader_id)) if !loader_id.is_empty() => {
            Ok(NavigationExpectation::Document(DocumentIdentity {
                frame_id: frame_id.into(),
                loader_id: loader_id.clone(),
            }))
        }
        None => {
            if frame_id != start.document.frame_id {
                return Err("same-document navigation changed frame identity".into());
            }
            Ok(NavigationExpectation::Current)
        }
        _ => Err("browser navigation returned a malformed loaderId".into()),
    }
}

pub(super) async fn wait_for_document(
    session: &mut CdpSession,
    expected: NavigationExpectation,
    wait_until: &str,
    duration: Duration,
) -> Result<DocumentIdentity, String> {
    let deadline = Instant::now() + duration;
    loop {
        session.pump_events(Duration::from_millis(5)).await?;
        if !pending_dialog_values(session).is_empty() {
            return Err(
                "browser navigation is blocked by a JavaScript dialog; inspect browser.dialog"
                    .into(),
            );
        }
        let document = current_document(session).await?;
        let matches = match &expected {
            NavigationExpectation::Document(expected) => &document == expected,
            NavigationExpectation::Change(start) => {
                document != start.document
                    || session.observation.navigation_revision != start.revision
            }
            NavigationExpectation::Current => true,
        };
        if matches && (!session.observation.loading || session.observation.committed) {
            let revision = session.observation.document_revision;
            let ready = session
                .command(
                    "browser.observe",
                    "Runtime.evaluate",
                    json!({
                        "expression":"document.readyState", "returnByValue":true,
                    }),
                )
                .await?;
            let ready = remote_value(ready, "browser readiness")?;
            let ready = ready
                .as_str()
                .ok_or("browser readiness returned no readyState string")?;
            let reached = match wait_until {
                "interactive" => matches!(ready, "interactive" | "complete"),
                "complete" => ready == "complete",
                _ => return Err("browser readiness requires interactive or complete".into()),
            };
            let after = current_document(session).await?;
            if reached && after == document && revision == session.observation.document_revision {
                return Ok(document);
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("browser navigation timed out waiting for the dispatched document to become {wait_until}; no navigation was replayed"));
        }
        session
            .pump_events(
                Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
            )
            .await?;
    }
}

pub(super) async fn settle_after_action(
    session: &mut CdpSession,
    start: &NavigationStart,
) -> Result<(), String> {
    let classification = Instant::now() + Duration::from_millis(250);
    while Instant::now() < classification
        && session.observation.navigation_revision == start.revision
        && session.frames.navigation_revision == start.frame_revision
    {
        if !pending_dialog_values(session).is_empty() {
            return Ok(());
        }
        session.pump_events(Duration::from_millis(10)).await?;
    }
    if !pending_dialog_values(session).is_empty() {
        return Ok(());
    }
    if session.observation.loading || session.observation.navigation_revision != start.revision {
        wait_for_document(
            session,
            NavigationExpectation::Current,
            "interactive",
            session.navigation_timeout,
        )
        .await?;
    }
    let deadline = Instant::now() + session.navigation_timeout;
    while session.frames.loading.iter().any(|(route, frame)| {
        route.is_some()
            || session
                .observation
                .document
                .as_ref()
                .is_some_and(|document| &document.frame_id != frame)
    }) {
        if !pending_dialog_values(session).is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "browser child-frame navigation did not settle; the action was not replayed".into(),
            );
        }
        session.pump_events(Duration::from_millis(25)).await?;
    }
    Ok(())
}

pub(super) async fn settle_observation(
    session: &mut CdpSession,
) -> Result<DocumentIdentity, String> {
    arm_navigation(session).await?;
    let document = wait_for_document(
        session,
        NavigationExpectation::Current,
        "interactive",
        session.navigation_timeout,
    )
    .await?;
    let paint_deadline = Instant::now() + Duration::from_secs(5);
    while !session.observation.painted && Instant::now() < paint_deadline {
        if !pending_dialog_values(session).is_empty() {
            return Err("browser observation is blocked by a JavaScript dialog".into());
        }
        let paint = session.command("browser.observe", "Runtime.evaluate", json!({
            "expression":"({readyState: document.readyState, painted: performance.getEntriesByType('paint').some(entry => entry.name === 'first-paint' || entry.name === 'first-contentful-paint')})", "returnByValue":true,
        })).await?;
        let paint = remote_value(paint, "browser paint observation")?;
        if paint["painted"] == true {
            session.observation.painted = true;
        }
        if session.observation.painted || paint["readyState"] == "complete" {
            break;
        }
        session.pump_events(Duration::from_millis(25)).await?;
    }
    let quiet_ms = if session.observation.navigation_revision
        != session.observation.last_snapshot_navigation
    {
        50
    } else {
        100
    };
    let quiet_start = Instant::now();
    let quiet_deadline = quiet_start + Duration::from_millis(750);
    loop {
        let last_update = session
            .observation
            .last_ax_update
            .unwrap_or(quiet_start)
            .max(quiet_start);
        if Instant::now().saturating_duration_since(last_update) >= Duration::from_millis(quiet_ms)
            || Instant::now() >= quiet_deadline
        {
            break;
        }
        session.pump_events(Duration::from_millis(25)).await?;
    }
    if current_document(session).await? != document
        || (session.observation.loading && !session.observation.committed)
    {
        return Err(
            "browser document changed while waiting for an observation; request a fresh snapshot"
                .into(),
        );
    }
    session.observation.last_snapshot_navigation = session.observation.navigation_revision;
    Ok(document)
}

pub(super) async fn validate_ref(
    session: &mut CdpSession,
    reference: &BrowserRef,
) -> Result<(), String> {
    if reference.target_id != session.target_id || reference.frame_path.is_empty() {
        return Err(
            "browser ref has no exact target/frame provenance; call browser.snapshot".into(),
        );
    }
    validate_frames(session, &reference.frame_path).await
}

fn remote_value(result: Value, action: &str) -> Result<Value, String> {
    if let Some(exception) = result.get("exceptionDetails") {
        let description = exception
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| exception.get("text").and_then(Value::as_str))
            .unwrap_or("page JavaScript exception");
        return Err(format!("{action} failed: {}", truncate(description, 1000)));
    }
    result
        .get("result")
        .and_then(|result| result.get("value"))
        .cloned()
        .ok_or_else(|| format!("{action} returned no value"))
}

async fn call_element(
    session: &mut CdpSession,
    tool: &str,
    route: Option<&str>,
    object_id: &str,
    body: &str,
    options: Value,
) -> Result<Value, String> {
    let function =
        format!("async function(options) {{ {DOM_HELPERS}\n{ELEMENT_HELPERS}\n{body}\n}}");
    let result = session.command_in(tool, route, "Runtime.callFunctionOn", json!({
        "objectId":object_id, "functionDeclaration":function, "arguments":[{"value":options}],
        "returnByValue":true, "awaitPromise":true, "userGesture":true,
    })).await?;
    remote_value(result, tool)
}

async fn release_object(
    session: &mut CdpSession,
    tool: &str,
    route: Option<&str>,
    object_id: &str,
) -> Result<(), String> {
    session
        .command_in(
            tool,
            route,
            "Runtime.releaseObject",
            json!({"objectId":object_id}),
        )
        .await
        .map(|_| ())
}

pub(super) struct PointerPoint {
    pub root: (f64, f64),
    pub input: (f64, f64),
    route: FrameRoute,
    frames: Vec<FrameDocument>,
}
impl PointerPoint {
    pub fn root(x: f64, y: f64) -> Self {
        Self {
            root: (x, y),
            input: (x, y),
            route: FrameRoute {
                session_id: None,
                attachment_seq: 0,
            },
            frames: vec![],
        }
    }
    pub async fn dispatch(
        &self,
        session: &mut CdpSession,
        tool: &str,
        mut params: Value,
    ) -> Result<(), String> {
        if session.frames.route(self.route.session_id.as_deref())? != self.route
            || (params["type"] != "mouseReleased"
                && self.frames.iter().any(|frame| {
                    session
                        .frames
                        .revision(&frame.route, &frame.document.frame_id)
                        != (frame.revision, frame.context_revision)
                }))
        {
            return Err("pointer frame changed before dispatch; call browser.snapshot".into());
        }
        if params["type"] == "mouseMoved" {
            session.move_cursor(tool, self.root.0, self.root.1, params["buttons"].as_u64().unwrap_or(0) == 0).await?;
            if !self.frames.is_empty() { validate_frames(session, &self.frames).await?; }
        }
        params["x"] = json!(self.input.0);
        params["y"] = json!(self.input.1);
        session
            .command_in(
                tool,
                self.route.session_id.as_deref(),
                "Input.dispatchMouseEvent",
                params,
            )
            .await
            .map(|_| ())
    }
}

pub(super) async fn pointer_point(
    session: &mut CdpSession,
    reference: &BrowserRef,
    tool: &str,
    enabled: bool,
) -> Result<PointerPoint, String> {
    validate_ref(session, reference).await?;
    let frame = reference
        .frame_path
        .last()
        .ok_or("browser ref has no frame")?;
    scroll_frame_owners(session, tool, &reference.frame_path).await?;
    let route = frame.route.session_id.as_deref();
    let object = frame_object(session, tool, frame, reference.backend_node_id).await?;
    let prepared = prepare_element(
        session,
        tool,
        route,
        &object,
        POINTER_PREPARE,
        json!({"enabled":enabled}),
    )
    .await;
    let released = release_object(session, tool, route, &object).await;
    let point = prepared?;
    released?;
    let point = (finite(&point, "x")?, finite(&point, "y")?);
    let point = top_level_point(session, tool, &reference.frame_path, point).await?;
    validate_ref(session, reference).await?;
    Ok(PointerPoint {
        root: point.0,
        input: point.1,
        route: frame.route.clone(),
        frames: reference.frame_path.clone(),
    })
}

async fn prepare_element(
    session: &mut CdpSession,
    tool: &str,
    route: Option<&str>,
    object_id: &str,
    body: &str,
    options: Value,
) -> Result<Value, String> {
    let deadline = Instant::now() + session.request_timeout;
    loop {
        let result = call_element(session, tool, route, object_id, body, options.clone()).await?;
        match result["status"].as_str() {
            Some("ready") => return Ok(result),
            Some("detached") => {
                return Err(format!(
                    "{tool} element is no longer connected; call browser.snapshot"
                ))
            }
            Some("retry") => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{tool} actionability timed out: {}",
                        result["reason"]
                            .as_str()
                            .unwrap_or("element is not actionable")
                    ));
                }
                session
                    .pump_events(
                        Duration::from_millis(100)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    )
                    .await?;
            }
            _ => return Err(format!("{tool} returned malformed actionability evidence")),
        }
    }
}

pub(super) async fn type_into_ref(
    session: &mut CdpSession,
    reference: &BrowserRef,
    text: &str,
    clear: bool,
    submit: bool,
) -> Result<(), String> {
    let tool = "browser.type";
    validate_ref(session, reference).await?;
    let frame = reference
        .frame_path
        .last()
        .ok_or("browser ref has no frame")?;
    let route = frame.route.session_id.as_deref();
    scroll_frame_owners(session, tool, &reference.frame_path).await?;
    let object_id = frame_object(session, tool, frame, reference.backend_node_id).await?;
    let mut input_id = None;
    let result: Result<(), String> = async {
        prepare_element(session, tool, route, &object_id, INPUT_PREPARE, json!({})).await?;
        validate_ref(session, reference).await?;
        let function = format!("function() {{ {DOM_HELPERS} {ELEMENT_HELPERS} const target=retarget(this,'follow-label'); if(!target?.isConnected) throw new Error('Element is not connected'); return target; }}");
        let retargeted = session.command_in(tool, route, "Runtime.callFunctionOn", json!({
            "objectId":object_id,"functionDeclaration":function,"returnByValue":false,
        })).await?;
        if retargeted.get("exceptionDetails").is_some() {
            remote_value(retargeted, tool)?;
            return Err("browser.type could not resolve its input target".into());
        }
        input_id = Some(retargeted.pointer("/result/objectId").and_then(Value::as_str)
            .ok_or("browser.type retargeted input has no object handle")?.to_string());
        let input = input_id.as_deref().ok_or("browser.type lost its input handle")?;
        let focused = call_element(
            session,
            tool, route,
            input,
            INPUT_FOCUS,
            json!({"clear":clear,"text":text}),
        )
        .await?;
        if focused["status"] != "focused" {
            return Err("browser.type focus did not reach the requested input".into());
        }
        if focused["needs_input"] == true && (!text.is_empty() || clear) {
            let focused = call_element(session, tool, route, input, INPUT_VERIFY, json!({})).await?;
            if focused != true {
                return Err(
                    "browser.type focus moved before text dispatch; text was not sent".into(),
                );
            }
            if text.is_empty() {
                dispatch_key_in(session, tool, route, "Backspace", 0, None).await?;
            } else {
                session
                    .command_in(tool, route, "Input.insertText", json!({"text":text}))
                    .await?;
            }
        }
        if submit {
            let focused = call_element(session, tool, route, input, INPUT_VERIFY, json!({})).await?;
            if focused != true {
                return Err("browser.type focus moved before submit; Enter was not sent".into());
            }
            dispatch_key_in(session, tool, route, "Enter", 0, None).await?;
        }
        Ok(())
    }
    .await;
    let input_released = match input_id {
        Some(input) => release_object(session, tool, route, &input).await,
        None => Ok(()),
    };
    let released = release_object(session, tool, route, &object_id).await;
    result?;
    input_released?;
    released
}

pub(super) async fn select_from_ref(
    session: &mut CdpSession,
    reference: &BrowserRef,
    values: &[String],
) -> Result<Value, String> {
    let tool = "browser.select";
    validate_ref(session, reference).await?;
    let frame = reference
        .frame_path
        .last()
        .ok_or("browser ref has no frame")?;
    let route = frame.route.session_id.as_deref();
    scroll_frame_owners(session, tool, &reference.frame_path).await?;
    let object_id = frame_object(session, tool, frame, reference.backend_node_id).await?;
    let result = async {
        prepare_element(
            session,
            tool,
            route,
            &object_id,
            "return states(this,true,false) || {status:'ready'};",
            json!({}),
        )
        .await?;
        validate_ref(session, reference).await?;
        call_element(
            session,
            tool,
            route,
            &object_id,
            SELECT_OPTIONS,
            json!({"values":values}),
        )
        .await
    }
    .await;
    let released = release_object(session, tool, route, &object_id).await;
    let selected = result?;
    released?;
    if !selected
        .as_array()
        .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err("browser.select returned malformed selected values".into());
    }
    Ok(selected)
}

const SELECT_OPTIONS: &str = r###"
const failure=states(this,true,false);
if (failure) throw new Error(failure.reason || 'Element is not connected');
const element=retarget(this,'follow-label');
if (element.nodeName.toLowerCase() !== 'select') throw new Error('Element is not a select');
const selected=[];
let remaining=options.values.slice();
for (const option of element.options) {
  if (!remaining.includes(option.value)) continue;
  if (et(option)) throw new Error('Option is not enabled');
  selected.push(option);
  if (element.multiple) remaining=remaining.filter(value=>value!==option.value);
  else { remaining=[]; break; }
}
if (remaining.length) throw new Error('Requested options were not found');
element.value=undefined;
for (const option of selected) option.selected=true;
element.dispatchEvent(new Event('input',{bubbles:true,composed:true}));
element.dispatchEvent(new Event('change',{bubbles:true}));
return selected.map(option=>option.value);
"###;

const ELEMENT_HELPERS: &str = r###"
function retarget(node, behavior) {
  let element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
  if (!element || behavior === "none") return element;
  if (!element.matches("input, textarea, select") && !element.isContentEditable)
    element = element.closest("button, [role=button], [role=checkbox], [role=radio]") || element;
  if (
    behavior === "follow-label" &&
    !element.matches(
      "a, input, textarea, button, select, [role=link], [role=button], [role=checkbox], [role=radio]",
    ) &&
    !element.isContentEditable
  ) {
    const label = element.closest("label");
    if (label && label.control) element = label.control;
  }
  return element;
}
function states(node, enabled, editable) {
  const visible = retarget(node, "none");
  const element = retarget(node, "follow-label");
  if (!visible?.isConnected || !element?.isConnected) return { status: "detached" };
  if (visible.ownerDocument !== document || element.ownerDocument !== document) throw new Error('Element is outside its exact frame execution context');
  if (!Q(visible)) return { status: "retry", reason: "Element is not visible" };
  if (enabled && et(element)) return { status: "retry", reason: "Element is not enabled" };
  if (editable) {
    const readonly = Pi(element);
    if (readonly === "error")
      throw new Error(
        "Element is not an input, textarea, select, contenteditable or role allowing aria-readonly",
      );
    if (readonly) return { status: "retry", reason: "Element is not editable" };
  }
  return null;
}
async function stableRect(element) {
  let rect = element.getBoundingClientRect(),
    stable = 0;
  for (let attempt = 0; attempt < 10; attempt++) {
    await new Promise((resolve) =>
      element.ownerDocument.defaultView.requestAnimationFrame(resolve),
    );
    const next = element.getBoundingClientRect();
    if (
      rect.left === next.left &&
      rect.top === next.top &&
      rect.width === next.width &&
      rect.height === next.height
    )
      stable++;
    else stable = 0;
    rect = next;
    if (stable >= 2) return rect;
  }
  return null;
}
function receivesPoint(point, target) {
  const roots = [];
  for (let element = target; element;) {
    const root = Ar(element);
    if (!root) break;
    roots.push(root);
    if (root.nodeType === 9) break;
    element = root.host;
  }
  let hit;
  for (let i = roots.length - 1; i >= 0; i--) {
    const root = roots[i],
      hits = root.elementsFromPoint(point.x, point.y),
      first = root.elementFromPoint(point.x, point.y);
    if (
      first &&
      hits[0] &&
      $(first) === hits[0] &&
      first.ownerDocument.defaultView.getComputedStyle(first).display === "contents"
    )
      hits.unshift(first);
    if (hits[0] && hits[0].shadowRoot === root && hits[1] === first) hits.shift();
    if (!hits[0]) return false;
    hit = hits[0];
    if (i && hit !== roots[i - 1].host) break;
  }
  for (; hit && hit !== target; hit = hit.assignedSlot ?? $(hit)) {}
  return hit === target;
}
"###;

const POINTER_PREPARE: &str = r###"
const element = retarget(this, 'none');
let last = 'Element does not receive pointer events';
for (const alignment of [{block:'center',inline:'nearest'}, {block:'end',inline:'end'}, {block:'start',inline:'start'}]) {
  let failure = states(element, options.enabled, false);
  if (failure) return failure;
  element.scrollIntoView({...alignment, behavior:'instant'});
  const rect = await stableRect(element);
  failure = states(element, options.enabled, false);
  if (failure) return failure;
  if (!rect) return {status:'retry',reason:'Element geometry is not stable'};
  if (rect.width <= 0 || rect.height <= 0) return {status:'retry',reason:'Element has no clickable bounding box'};
  const point = {x:Math.max(0,rect.left + rect.width/2),y:Math.max(0,rect.top + rect.height/2)};
  if (!receivesPoint(point, element)) continue;
  return {status:'ready',...point};
}
return {status:'retry',reason:last};
"###;

const INPUT_PREPARE: &str = r###"
const failure = states(this, true, true);
return failure || {status:'ready'};
"###;

const INPUT_FOCUS: &str = r###"
const failure = states(this, true, true);
if (failure) throw new Error(failure.reason || 'Element is not connected');
const element = retarget(this, 'follow-label');
element.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'});
if (options.clear && element.nodeName.toLowerCase() === 'input') {
  const type = element.type.toLowerCase();
  const setValue = new Set(['color','date','time','datetime-local','month','range','week']);
  if (!new Set(['','email','number','password','search','tel','text','url']).has(type) && !setValue.has(type)) throw new Error(`Input of type "${type}" cannot be filled`);
  if (type === 'number' && isNaN(Number(options.text.trim()))) throw new Error('Cannot type text into input[type=number]');
  if (setValue.has(type)) {
    const value = type === 'color' ? options.text.trim().toLowerCase() : options.text.trim();
    element.focus();
    if (element.getRootNode().activeElement !== element) throw new Error('Input did not retain focus');
    element.value = value;
    if (element.value !== value) throw new Error('Malformed input value');
    element.dispatchEvent(new Event('input',{bubbles:true,composed:true}));
    element.dispatchEvent(new Event('change',{bubbles:true}));
    return {status:'focused',needs_input:false};
  }
}
if (!element.matches('input,textarea') && !element.isContentEditable) throw new Error('Element is not a text input, textarea or contenteditable');
if (options.clear) {
  if (element.nodeName.toLowerCase() === 'input') { element.select(); element.focus(); }
  else if (element.nodeName.toLowerCase() === 'textarea') { element.selectionStart=0; element.selectionEnd=element.value.length; element.focus(); }
  else {
    element.focus();
    const range=element.ownerDocument.createRange(); range.selectNodeContents(element);
    const selection=element.ownerDocument.defaultView.getSelection();
    if (!selection) throw new Error('Editable target has no selection');
    selection.removeAllRanges(); selection.addRange(range);
  }
} else if (!element.matches(':focus')) {
  const active=element.getRootNode().activeElement;
  if (element.isContentEditable && active?.blur) active.blur();
  element.focus(); element.focus();
}
if (!element.isConnected || element.getRootNode().activeElement !== element) throw new Error('Input did not retain focus');
return {status:'focused',needs_input:true};
"###;

const INPUT_VERIFY: &str = r###"
const element=retarget(this,'follow-label');
return !!element?.isConnected && element.getRootNode().activeElement === element && !states(this,true,true);
"###;

fn finite(value: &Value, key: &str) -> Result<f64, String> {
    value[key]
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("frame geometry has no finite {key}"))
}

async fn owner_geometry(
    session: &mut CdpSession,
    tool: &str,
    parent: &FrameDocument,
    child: &FrameDocument,
    scroll: bool,
) -> Result<Value, String> {
    let route = parent.route.session_id.as_deref();
    let backend = child
        .owner_backend_node_id
        .ok_or("iframe has no exact DOM owner")?;
    let owner = session
        .command_in(
            tool,
            route,
            "DOM.getFrameOwner",
            json!({"frameId":child.document.frame_id}),
        )
        .await?;
    if owner["backendNodeId"].as_i64() != Some(backend) {
        return Err("iframe owner changed since snapshot".into());
    }
    let object = frame_object(session, tool, parent, backend).await?;
    let geometry = prepare_element(
        session,
        tool,
        route,
        &object,
        FRAME_GEOMETRY,
        json!({"scroll":scroll}),
    )
    .await;
    let released = release_object(session, tool, route, &object).await;
    let geometry = geometry?;
    released?;
    Ok(geometry)
}

async fn scroll_frame_owners(
    session: &mut CdpSession,
    tool: &str,
    path: &[FrameDocument],
) -> Result<(), String> {
    for pair in path.windows(2) {
        owner_geometry(session, tool, &pair[0], &pair[1], true).await?;
    }
    Ok(())
}

async fn top_level_point(
    session: &mut CdpSession,
    tool: &str,
    path: &[FrameDocument],
    mut point: (f64, f64),
) -> Result<((f64, f64), (f64, f64)), String> {
    let mut input = None;
    let mut geometry = Vec::new();
    for pair in path.windows(2) {
        geometry.push(owner_geometry(session, tool, &pair[0], &pair[1], false).await?);
    }
    for index in (1..path.len()).rev() {
        let parent = &path[index - 1];
        let child = &path[index];
        let shape = &geometry[index - 1];
        if child.route == parent.route {
            point = (
                finite(shape, "left")? + point.0 * finite(shape, "scaleX")?,
                finite(shape, "top")? + point.1 * finite(shape, "scaleY")?,
            );
        } else {
            input.get_or_insert(point);
            let quads = session
                .command_in(
                    tool,
                    parent.route.session_id.as_deref(),
                    "DOM.getContentQuads",
                    json!({"backendNodeId":child.owner_backend_node_id}),
                )
                .await?;
            let quad = quads["quads"]
                .as_array()
                .and_then(|quads| quads.first())
                .and_then(Value::as_array)
                .filter(|quad| quad.len() == 8)
                .ok_or("iframe has no content quad")?;
            let numbers = quad
                .iter()
                .map(|value| {
                    value
                        .as_f64()
                        .filter(|number| number.is_finite())
                        .ok_or("iframe content quad is not finite")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let width = finite(shape, "width")?;
            let height = finite(shape, "height")?;
            if width <= 0.0 || height <= 0.0 {
                return Err("iframe content viewport is empty".into());
            }
            point = (
                numbers[0]
                    + point.0 / width * (numbers[2] - numbers[0])
                    + point.1 / height * (numbers[6] - numbers[0]),
                numbers[1]
                    + point.0 / width * (numbers[3] - numbers[1])
                    + point.1 / height * (numbers[7] - numbers[1]),
            );
            let hit=session.command_in(tool,parent.route.session_id.as_deref(),"DOM.getNodeForLocation",json!({"x":point.0.round(),"y":point.1.round(),"includeUserAgentShadowDOM":true})).await?;
            if hit["backendNodeId"].as_i64() != child.owner_backend_node_id
                && hit["frameId"] != child.document.frame_id
            {
                return Err("another element intercepts the iframe pointer boundary".into());
            }
            let mut start = index - 1;
            while start > 0 && path[start - 1].route == parent.route {
                start -= 1;
            }
            for edge in start..index - 1 {
                let shape = &geometry[edge];
                point = (
                    (point.0 - finite(shape, "left")?) / finite(shape, "scaleX")?,
                    (point.1 - finite(shape, "top")?) / finite(shape, "scaleY")?,
                );
            }
        }
        if !point.0.is_finite() || !point.1.is_finite() {
            return Err("iframe point translation is not finite".into());
        }
        let route = parent.route.session_id.as_deref();
        let object = frame_object(
            session,
            tool,
            parent,
            child
                .owner_backend_node_id
                .ok_or("iframe owner disappeared")?,
        )
        .await?;
        let hit = call_element(
            session,
            tool,
            route,
            &object,
            "return !states(this,false,false) && receivesPoint(options,this);",
            json!({"x":point.0,"y":point.1}),
        )
        .await;
        let released = release_object(session, tool, route, &object).await;
        if hit? != true {
            return Err(
                "iframe ancestor does not receive pointer events at the target point".into(),
            );
        }
        released?;
    }
    Ok((point, input.unwrap_or(point)))
}

const FRAME_GEOMETRY: &str = r###"
const failure=states(this,false,false);
if (failure) return failure;
if (options.scroll) this.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'});
const rect=await stableRect(this);
if (!rect) return {status:'retry',reason:'Frame geometry is not stable'};
const offsetWidth=Number(this.offsetWidth)||rect.width;
const offsetHeight=Number(this.offsetHeight)||rect.height;
const width=Number(this.clientWidth),height=Number(this.clientHeight);
if (rect.width<=0 || rect.height<=0 || offsetWidth<=0 || offsetHeight<=0 || width<=0 || height<=0) return {status:'retry',reason:'Frame has no actionable content box'};
const scaleX=rect.width/offsetWidth,scaleY=rect.height/offsetHeight;
return {status:'ready',left:rect.left+(Number(this.clientLeft)||0)*scaleX,top:rect.top+(Number(this.clientTop)||0)*scaleY,scaleX,scaleY,width,height};
"###;

pub(crate) fn ordered_ax_nodes(nodes: &[Value]) -> Result<Vec<Value>, String> {
    let mut index = BTreeMap::new();
    let mut unique_ids = Vec::new();
    let mut children = BTreeSet::new();
    for node in nodes {
        let id = node["nodeId"].as_str().ok_or("AX node has no identity")?;
        if let Some(existing) = index.get(id) {
            if *existing != node {
                return Err("AX node identity is duplicated".into());
            }
            continue;
        }
        index.insert(id, node);
        unique_ids.push(id);
        for child in node["childIds"].as_array().into_iter().flatten() {
            children.insert(child.as_str().ok_or("AX child is malformed")?);
        }
    }
    let mut pending = unique_ids
        .iter()
        .copied()
        .filter(|id| !children.contains(id))
        .rev()
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    let mut ordered = Vec::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            return Err("AX tree contains a cycle or multiple parents".into());
        }
        let Some(node) = index.get(id) else {
            continue;
        };
        ordered.push((*node).clone());
        pending.extend(
            node["childIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .rev(),
        );
    }
    if ordered.len() != index.len() {
        return Err("AX tree contains unreachable nodes".into());
    }
    Ok(ordered)
}
