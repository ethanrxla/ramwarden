// The warden's speech bubble in the popup, and "Analyze now".
//
// The daemon decides what the warden says (docs/specs/warden-voice.md); the
// popup only shows GET /warden/bubble and posts the user's choice. Pure
// rendering lives in wardenView() so it can be tested without a browser.

const WARDEN_MUTE = [["Quiet for an hour", 1], ["Quiet for today", 12], ["Quiet until I unmute", null]];

function wardenEsc(s) {
  return String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

// What to draw for a /warden/bubble reply: null hides the warden.
function wardenView(speech) {
  if (!speech || speech.pet === false) return null; // pets come with Plus and Pro
  const b = speech.bubble;
  if (!b) {
    return speech.muted
      ? { text: "Your warden is keeping quiet.", buttons: [{ id: "unmute", label: "Unmute" }], muted: true }
      : null;
  }
  const choices = b.choices || [];
  // The focus question is answered by typing. An older daemon sent a
  // "Tell Warden" button that only opened a box: the box is always there now.
  const input = b.input || (b.kind === "focus" || choices.some(c => c.id === "focus") ? "What are you working on?" : null);
  const buttons = choices.filter(c => c.id !== "focus").map(c => ({ id: c.id, label: c.label }));
  return { id: b.id, text: b.text, title: b.tab ? b.tab.url : "", buttons, input, muted: false, question: buttons.length > 0 || !!input };
}

function wardenHTML(view) {
  if (!view) return "";
  const buttons = view.buttons.map(b =>
    `<button class="warden-choice${b.id === "close" ? " danger" : ""}" data-choice="${wardenEsc(b.id)}">${wardenEsc(b.label)}</button>`).join("");
  const mute = view.muted ? "" : `<select class="warden-mute" title="Quiet, please"><option value="">🔊</option>${
    WARDEN_MUTE.map(([label, h], i) => `<option value="${i}">${wardenEsc(label)}</option>`).join("")}</select>`;
  const dismiss = view.id ? `<button class="warden-x" data-choice="dismiss" title="Dismiss">✕</button>` : "";
  // Enter submits the form; the arrow does the same.
  const input = view.input ? `<form class="warden-say"><input class="warden-input" maxlength="500" autocomplete="off"
    placeholder="${wardenEsc(view.input)}" aria-label="${wardenEsc(view.input)}"><button class="warden-send" title="Send (Enter)">➤</button></form>` : "";
  return `<div class="warden-ram" aria-hidden="true">🐏</div>
    <div class="warden-bubble"><div class="warden-text" title="${wardenEsc(view.title || "")}">${wardenEsc(view.text)}</div>
    <div class="warden-choices">${buttons}</div>${input}</div>${mute}${dismiss}`;
}

// The warden's answer to a focus, with the tabs it thinks belong to it as
// checkboxes. Nothing changes until "Use these preferences" is pressed.
function focusHTML(you, r) {
  const tabs = (r.proposal && r.proposal.tabs) || [];
  const list = tabs.map((t, i) => `<label class="warden-tab" title="${wardenEsc(t.url)}"><input type="checkbox" data-tab="${i}"${t.selected ? " checked" : ""}> ${wardenEsc(t.title || "Web page")}</label>`).join("");
  const keep = tabs.length ? `<div class="warden-note">Relevant to this task</div><div class="warden-tabs">${list}</div>
    <div class="warden-choices"><button class="warden-choice" data-act="focus-keep">Use these preferences</button></div>` : "";
  return `<div class="warden-ram" aria-hidden="true">🐏</div><div class="warden-bubble">
    <div class="warden-note">You: ${wardenEsc(you)}</div><div class="warden-text">${wardenEsc(r.reply)}</div>${keep}</div>
    <button class="warden-x" data-act="focus-done" title="Put this away">✕</button>`;
}

// The "Analyze now" result: the warden's words plus buttons for what it suggests.
function analysisHTML(a) {
  const n = (l) => (l || []).length;
  const unload = n(a.unload) ? `<button class="warden-choice" data-act="unload">Unload ${n(a.unload)} tab${n(a.unload) > 1 ? "s" : ""}</button>` : "";
  const close = n(a.close) ? `<button class="warden-choice danger" data-act="close">Close ${n(a.close)} tab${n(a.close) > 1 ? "s" : ""}</button>` : "";
  const note = a.tier === "facts" ? `<div class="warden-note">Rules only: no AI model answered.</div>` : "";
  return `<div class="warden-ram" aria-hidden="true">🐏</div><div class="warden-bubble">
    <div class="warden-text">${wardenEsc(a.reply)}</div><div class="warden-choices">${unload}${close}</div>${note}</div>`;
}

if (typeof module !== "undefined") module.exports = { wardenView, wardenHTML, analysisHTML, focusHTML };
