// A small element builder. The screen redraws its body from the snapshot, so
// building nodes directly is all it needs.

export type Child = Node | string | number | null | undefined | false;
type Value = string | number | boolean | null | undefined;
type Handler = (event: Event) => void;
export type Props = Record<string, Value | Handler>;

/**
 * `h('button', { class: 'x', disabled: true, onClick: fn }, '라벨')`.
 * `on*` props with a function value become listeners; `true` sets an empty
 * attribute, `false`, `null` and `undefined` leave it out.
 */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: Props | null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  for (const [name, value] of Object.entries(props ?? {})) {
    if (typeof value === 'function') {
      element.addEventListener(name.slice(2).toLowerCase(), value);
    } else if (value === true) {
      element.setAttribute(name, '');
    } else if (value !== false && value !== null && value !== undefined) {
      element.setAttribute(name, String(value));
    }
  }
  append(element, children);
  return element;
}

export function append(parent: Node, children: Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    parent.appendChild(typeof child === 'object' ? child : document.createTextNode(String(child)));
  }
}

/** A text node and a setter that touches it only when the text changes. */
export function liveText(initial = ''): { node: Text; set(value: string): void } {
  const node = document.createTextNode(initial);
  return {
    node,
    set(value: string) {
      if (node.data !== value) node.data = value;
    },
  };
}

/** Rows of facts as a definition list. */
export function facts(rows: [string, Child][], className = 'facts'): HTMLDListElement {
  const list = h('dl', { class: className });
  for (const [label, value] of rows) {
    list.append(h('div', { class: 'fact' }, h('dt', null, label), h('dd', null, value)));
  }
  return list;
}
