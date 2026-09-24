// Which repositories a project board serves, and New scratch's repository
// picker (#499): `node --test chrome-extension/test/`.
import { test } from "node:test";
import assert from "node:assert/strict";

/// Just enough of a DOM for the form to be drawn and read back.
class Node {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.dataset = {};
    this.attributes = {};
    this.textContent = "";
    this.className = "";
  }
  append(...nodes) {
    this.children.push(...nodes.filter((one) => typeof one === "object"));
  }
  prepend(node) {
    this.children.unshift(node);
  }
  setAttribute(name, value) {
    this.attributes[name] = value;
  }
  *all() {
    for (const child of this.children) {
      yield child;
      yield* child.all();
    }
  }
}
globalThis.document = { createElement: (tag) => new Node(tag) };
// Asks are never answered: the form is read before any factory replies.
globalThis.chrome = { runtime: { sendMessage: () => new Promise(() => {}) } };
await import("../writes-form.js");
const { projectKey, projectRepos, renderScratch } = globalThis.ssfWrites;

test("a project is its owner and number, whatever view or host names it", () => {
  assert.equal(projectKey("https://github.com/users/MikeKelly/projects/5"), "mikekelly/5");
  assert.equal(projectKey("/users/mikekelly/projects/5/views/1"), "mikekelly/5");
  assert.equal(projectKey("/orgs/Acme/projects/12?pane=issue"), "acme/12");
  assert.equal(projectKey("/acme/widgets/projects/12"), "acme/12");
  assert.equal(projectKey("/acme/widgets"), null);
  assert.equal(projectKey("/acme/widgets/issues/3"), null);
});

test("a project page serves every repository linked to it, and no other", () => {
  const linked = {
    "o/a": ["https://github.com/users/o/projects/5"],
    "o/b": ["https://github.com/orgs/x/projects/1", "https://github.com/users/O/projects/5"],
    "o/c": ["https://github.com/users/o/projects/50"],
  };
  assert.deepEqual(projectRepos(linked, "/users/o/projects/5/views/2"), ["o/a", "o/b"]);
  assert.deepEqual(projectRepos(linked, "/orgs/x/projects/1"), ["o/b"]);
  assert.deepEqual(projectRepos(linked, "/users/o/projects/7"), []);
  // An older factory publishes no projects: nothing is served.
  assert.deepEqual(projectRepos(undefined, "/users/o/projects/5"), []);
});

const factory = { url: "http://f", label: "f" };

/// The New scratch form's pickers, by label, after New scratch is clicked.
function pickers(repos) {
  const first = renderScratch({ factories: [factory], repos, login: "me", sessions: [] });
  const create = [...first.all()].find(
    (one) => one.tagName === "BUTTON" && one.textContent === "New scratch",
  );
  create?.onclick();
  const form = renderScratch({ factories: [factory], repos, login: "me", sessions: [] });
  const fields = [...form.all()].filter((one) => one.dataset.ssfNode?.startsWith("field:"));
  return new Map(
    fields.map((field) => [
      field.dataset.ssfNode.slice("field:".length),
      field.children.find((one) => one.tagName === "SELECT"),
    ]),
  );
}

test("New scratch over several repositories asks which, and keeps the choice", () => {
  const fields = pickers(["o/a", "o/b"]);
  const box = fields.get("Repository");
  assert.ok(box, "a repository picker");
  assert.deepEqual(
    box.children.map((one) => one.value),
    ["o/a", "o/b"],
  );
  assert.equal(box.value, "o/a");
  box.onchange({ currentTarget: { value: "o/b" } });
  assert.equal(pickers(["o/a", "o/b"]).get("Repository").value, "o/b");
});

test("New scratch on one repository has no repository picker", () => {
  const fields = pickers(["o/solo"]);
  assert.ok(fields.has("Harness"));
  assert.equal(fields.has("Repository"), false);
});
