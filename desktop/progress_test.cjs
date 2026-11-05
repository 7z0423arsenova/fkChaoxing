const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

// Exercise the page's actual renderer with a minimal DOM, without requiring a WebView.
class Element {
  constructor() { this.children = []; this.attributes = {}; }
  append(...nodes) {
    for (const node of nodes) { node.parent = this; this.children.push(node); }
  }
  remove() { this.parent.children = this.parent.children.filter((node) => node !== this); }
  setAttribute(name, value) { this.attributes[name] = value; }
  removeAttribute(name) { delete this.attributes[name]; }
}

test('concurrent progress reuses each task row, including identical labels', () => {
  const list = new Element();
  const page = readFileSync(`${__dirname}/page.html`, 'utf8');
  const start = page.indexOf('    const taskProgressList =');
  const end = page.indexOf('    let runToken =', start);
  assert.ok(start >= 0 && end > start);
  const context = vm.createContext({ document: {
    getElementById: () => list,
    createElement: () => new Element(),
  }});
  vm.runInContext(page.slice(start, end) + '\nglobalThis.render = renderTaskProgress;', context);
  const render = context.render;
  const item = (id, position, total = 100) => ({ id, label: '相同课程 / 相同视频', position, total });
  render([item('1', 10), item('2', 20)]);
  const [first, second] = list.children;
  for (let position = 11; position <= 80; position++) {
    render([item('1', position), item('2', 20)]);
  }
  assert.equal(list.children.length, 2);
  assert.equal(list.children[0], first);
  assert.equal(list.children[1], second);
  assert.equal(first.children[2].value, 80);
  assert.equal(second.children[2].value, 20);
  assert.equal(first.children[1].textContent, '80% · 80 / 100');
  render([item('2', 30)]);
  assert.equal(list.children.length, 1);
  assert.equal(list.children[0], second);
  render();
  assert.equal(list.children.length, 0);
  assert.equal(list.hidden, true);
  render([item('3', 0, 0)]);
  assert.equal(list.children[0].children[1].textContent, '正在读取…');
  assert.equal(list.hidden, false);
});
