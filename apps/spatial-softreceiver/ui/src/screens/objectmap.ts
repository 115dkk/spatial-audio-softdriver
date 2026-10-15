// The object map: the listening place seen from above, the output layout's
// directions around it and, while running, the sound objects of the last
// mixed block (EngineStatus.scene). The drawing is built once; the 30 Hz
// meters only move a fixed pool of circles, so a frame never builds nodes.

import type { DirectionView, EngineStatus, SceneObject } from '../bindings';
import { h, liveText, setAttr, svg } from '../dom';
import { text } from '../text';
import { panel } from './common';

const p = text.profile;

/**
 * Circles made up front: every lane the scene can name (the contract's four
 * endpoint slots) times the objects a lane carries (`ssr_types::MAX_OBJECTS`),
 * which is the engine's own bound (`MAX_SCENE_OBJECTS`).
 */
const SCENE_LANES = 4;
const OBJECTS_PER_LANE = 64;
export const MAP_POOL = SCENE_LANES * OBJECTS_PER_LANE;

/** Lanes with their own colour and legend entry: the endpoints the screen shows. */
const COLOURED_LANES = 3;

/** viewBox units: the drawing is a square, the horizon ring inside it. */
const SIZE = 300;
const CENTER = SIZE / 2;
const RADIUS = 112;
const LABEL_GAP = 18;
/** How far from the centre a name stays, clear of the listening place. */
const LISTENER_CLEAR = 28;

/** Levels at or under the floor draw small and faint; 0 dBFS draws largest. */
const FLOOR_DB = -60;
const OBJECT_R_MIN = 3;
const OBJECT_R_MAX = 11;
const OPACITY_MIN = 0.35;

/** A vector this far up (about 15°) counts as above the listening place. */
const UP_Z = 0.25;

const round1 = (value: number) => (Math.round(value * 10) / 10).toString();

/** A rendering vector (x right, y front, z up) on the drawing's plane. */
function toPlane(x: number, y: number): [cx: number, cy: number] {
  // Only the renderer's unit vectors are expected; anything longer sits on
  // the horizon ring rather than off the drawing.
  const length = Math.hypot(x, y);
  const scale = length > 1 ? 1 / length : 1;
  return [CENTER + x * scale * RADIUS, CENTER - y * scale * RADIUS];
}

/** A direction's vector: azimuth 0° is ahead and grows to the left. */
function directionVector(direction: DirectionView): [x: number, y: number, z: number] {
  const azimuth = (direction.azimuth_deg * Math.PI) / 180;
  const elevation = (direction.elevation_deg * Math.PI) / 180;
  return [
    -Math.sin(azimuth) * Math.cos(elevation),
    Math.cos(azimuth) * Math.cos(elevation),
    Math.sin(elevation),
  ];
}

function laneClass(lane: number): string {
  return lane < COLOURED_LANES ? `lane-${lane}` : 'lane-other';
}

function sameDirections(a: readonly DirectionView[], b: readonly DirectionView[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  return a.every((direction, index) => {
    const other = b[index];
    return (
      other !== undefined &&
      direction.id === other.id &&
      direction.name === other.name &&
      direction.azimuth_deg === other.azimuth_deg &&
      direction.elevation_deg === other.elevation_deg
    );
  });
}

/** A direction's point and its name; the live objects draw over both. */
function directionMark(direction: DirectionView): { point: SVGGElement; name: SVGTextElement } {
  const [x, y, z] = directionVector(direction);
  const [cx, cy] = toPlane(x, y);
  const up = z >= UP_Z;
  // A name sits outward from its point, past the horizon ring; a raised
  // point's name sits inward, clear of the bed's points and objects, unless
  // that would reach the listening place. Under the point when overhead.
  const length = Math.hypot(x, y);
  const inward = up && length * RADIUS - LABEL_GAP > LISTENER_CLEAR;
  const sign = inward ? -1 : 1;
  const [ux, uy] = length < 0.05 ? [0, -1] : [(sign * x) / length, (sign * y) / length];
  const upClass = up ? ' is-up' : '';
  return {
    point: svg(
      'g',
      { class: `map-direction${upClass}`, 'data-direction': direction.id },
      svg('title', null, `${direction.name} · ${p.angles(direction.azimuth_deg, direction.elevation_deg)}`),
      svg('circle', { cx: round1(cx), cy: round1(cy), r: 3.5 }),
    ),
    name: svg(
      'text',
      {
        class: `map-label${upClass}`,
        'data-direction': direction.id,
        x: round1(cx + ux * LABEL_GAP),
        y: round1(cy - uy * LABEL_GAP),
        'text-anchor': 'middle',
        'dominant-baseline': 'central',
        'aria-hidden': 'true',
      },
      direction.name,
    ),
  };
}

/** The listening place: a head with its nose to the front. */
function listener(): SVGGElement {
  return svg(
    'g',
    { class: 'map-listener', 'aria-hidden': 'true' },
    svg('path', { d: `M ${CENTER - 6} ${CENTER - 5} L ${CENTER} ${CENTER - 19} L ${CENTER + 6} ${CENTER - 5} Z` }),
    svg('circle', { cx: CENTER, cy: CENTER, r: 9 }),
  );
}

/** Colour by entrance on one line, outline by height on the next. */
function legend(): HTMLElement {
  const item = (swatch: string, label: string) =>
    h('li', null, h('span', { class: `map-swatch ${swatch}`, 'aria-hidden': 'true' }), label);
  return h(
    'div',
    { class: 'map-legend', role: 'group', 'aria-label': p.legendLabel },
    h(
      'ul',
      null,
      ...text.endpoints.slice(0, COLOURED_LANES).map((endpoint, lane) => item(`lane-${lane}`, endpoint.name)),
    ),
    h('ul', null, item('is-flat', p.legendFlat), item('is-up', p.legendUp)),
  );
}

export interface ObjectMap {
  element: HTMLElement;
  /** Moves the circles to the scene and follows a changed layout. */
  update(engine: EngineStatus): void;
}

/**
 * The map panel. `directions` are drawn now; with `running`, `update` places
 * the scene's objects (the lane picks the colour, the level the size and
 * strength, the height the outline). Without it only the directions show.
 */
export function objectMap(directions: readonly DirectionView[], running: boolean): ObjectMap {
  const pointLayer = svg('g', { class: 'map-directions' });
  const nameLayer = svg('g', { class: 'map-labels' });
  let drawnDirections: readonly DirectionView[] = [];
  const drawDirections = (next: readonly DirectionView[]) => {
    if (sameDirections(next, drawnDirections)) return;
    drawnDirections = next;
    const marks = next.map(directionMark);
    pointLayer.replaceChildren(...marks.map((mark) => mark.point));
    nameLayer.replaceChildren(...marks.map((mark) => mark.name));
  };
  drawDirections(directions);

  const circles = Array.from({ length: MAP_POOL }, () =>
    svg('circle', { class: 'map-object', cx: CENTER, cy: CENTER, r: OBJECT_R_MIN, display: 'none' }),
  );
  const objectLayer = svg('g', { class: 'map-objects' }, ...circles);
  let shown = 0;

  const drawing = svg(
    'svg',
    {
      class: 'map-drawing',
      viewBox: `0 0 ${SIZE} ${SIZE}`,
      role: 'img',
      'aria-label': p.mapLabel,
    },
    svg('circle', { class: 'map-disc', cx: CENTER, cy: CENTER, r: RADIUS }),
    svg('circle', { class: 'map-ring', cx: CENTER, cy: CENTER, r: round1(RADIUS * Math.SQRT1_2) }),
    svg('line', { class: 'map-axis', x1: CENTER, y1: CENTER - RADIUS, x2: CENTER, y2: CENTER + RADIUS }),
    svg('line', { class: 'map-axis', x1: CENTER - RADIUS, y1: CENTER, x2: CENTER + RADIUS, y2: CENTER }),
    pointLayer,
    nameLayer,
    listener(),
    objectLayer,
  );

  const status = liveText(running ? p.mapObjects(0) : p.mapOff);
  const element = panel(
    p.mapTitle,
    [
      h('p', { class: 'hint' }, p.mapBody),
      h('div', { class: 'map-frame' }, drawing),
      h('p', { class: 'map-status' }, status.node),
      legend(),
    ],
    'object-map',
  );

  function place(scene: readonly SceneObject[]) {
    const count = Math.min(scene.length, MAP_POOL);
    for (let slot = 0; slot < count; slot += 1) {
      const object = scene[slot] as SceneObject;
      const circle = circles[slot] as SVGCircleElement;
      const [cx, cy] = toPlane(object.x, object.y);
      const strength = Math.min(1, Math.max(0, (object.level_dbfs - FLOOR_DB) / -FLOOR_DB));
      setAttr(circle, 'cx', round1(cx));
      setAttr(circle, 'cy', round1(cy));
      setAttr(circle, 'r', round1(OBJECT_R_MIN + strength * (OBJECT_R_MAX - OBJECT_R_MIN)));
      setAttr(circle, 'opacity', (OPACITY_MIN + strength * (1 - OPACITY_MIN)).toFixed(2));
      setAttr(circle, 'class', `map-object ${laneClass(object.lane)} ${object.z >= UP_Z ? 'is-up' : 'is-flat'}`);
      if (slot >= shown) circle.removeAttribute('display');
    }
    for (let slot = count; slot < shown; slot += 1) circles[slot]?.setAttribute('display', 'none');
    shown = count;
    status.set(p.mapObjects(count));
  }

  return {
    element,
    update(engine) {
      if (!running) return;
      drawDirections(engine.directions);
      place(engine.scene);
    },
  };
}
