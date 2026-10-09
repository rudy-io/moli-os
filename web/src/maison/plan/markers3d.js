// The house's devices in 3D, each with a shape that says what it is: a lamp
// (shade and bulb, which shines), a wall switch (its rocker lights up when
// on), a socket, a camera, an air conditioner, a speaker or a TV, a sensor
// (a small puck). One `glow` material per marker carries its state.

import * as THREE from 'three';

/** Where each shape sits (m above its floor): lamps just under the cut of
 *  the doll's house walls, switches at hand height, sockets low. */
export const HEIGHT = { lamp: 1.45, switch: 1.1, plug: 0.45, camera: 1.4, climate: 1.4, media: 0.75, sensor: 1.2 };

const box = (w, h, d, mat) => new THREE.Mesh(new THREE.BoxGeometry(w, h, d), mat);

/**
 * A marker: { group, glow, body } (Materials owned by the marker, disposed
 * with it). The group's children carry `userData.device` for taps.
 */
export function marker(shape, device) {
  const group = new THREE.Group();
  const body = new THREE.MeshStandardMaterial({ color: 0xffffff, roughness: 0.55 });
  const glow = new THREE.MeshStandardMaterial({ color: 0xdddddd, emissive: 0x000000, roughness: 0.35 });
  const dark = new THREE.MeshStandardMaterial({ color: 0x2b2e33, roughness: 0.5 });
  const add = (mesh, x = 0, y = 0, z = 0) => {
    mesh.position.set(x, y, z);
    mesh.userData.device = device;
    group.add(mesh);
    return mesh;
  };
  // Flat things face the default camera (south-east).
  const facing = () => (group.rotation.y = Math.PI / 4);

  switch (shape) {
    case 'lamp': {
      const shade = new THREE.Mesh(new THREE.ConeGeometry(0.15, 0.13, 24, 1, true), body);
      shade.material.side = THREE.DoubleSide;
      add(shade, 0, 0.07, 0);
      add(new THREE.Mesh(new THREE.SphereGeometry(0.07, 18, 12), glow), 0, 0, 0);
      add(new THREE.Mesh(new THREE.CylinderGeometry(0.006, 0.006, 0.3, 6), dark), 0, 0.28, 0);
      break;
    }
    case 'switch': {
      add(box(0.16, 0.16, 0.025, body));
      add(box(0.065, 0.1, 0.03, glow), 0, 0, 0.012);
      facing();
      break;
    }
    case 'plug': {
      add(box(0.16, 0.16, 0.025, body));
      const socket = new THREE.Mesh(new THREE.CylinderGeometry(0.055, 0.055, 0.02, 24), glow);
      socket.rotation.x = Math.PI / 2;
      add(socket, 0, 0, 0.014);
      for (const x of [-0.02, 0.02]) add(new THREE.Mesh(new THREE.SphereGeometry(0.009, 8, 6), dark), x, 0, 0.026);
      facing();
      break;
    }
    case 'camera': {
      add(box(0.11, 0.09, 0.17, body));
      const lens = new THREE.Mesh(new THREE.CylinderGeometry(0.035, 0.035, 0.04, 16), dark);
      lens.rotation.x = Math.PI / 2;
      add(lens, 0, 0, 0.1);
      add(new THREE.Mesh(new THREE.SphereGeometry(0.012, 8, 6), glow), 0.035, 0.03, 0.086);
      facing();
      break;
    }
    case 'climate': {
      add(box(0.6, 0.18, 0.2, body));
      add(box(0.5, 0.02, 0.012, glow), 0, -0.06, 0.101);
      facing();
      break;
    }
    case 'media': {
      add(box(0.34, 0.07, 0.09, dark));
      add(box(0.03, 0.012, 0.01, glow), 0.12, 0, 0.046);
      facing();
      break;
    }
    default: {
      add(new THREE.Mesh(new THREE.CylinderGeometry(0.065, 0.075, 0.035, 20), body));
      add(new THREE.Mesh(new THREE.CylinderGeometry(0.035, 0.035, 0.012, 16), glow), 0, 0.022, 0);
    }
  }
  return { group, glow, body, materials: [body, glow, dark] };
}

const OFF = new THREE.Color(0x9a9a9a);
const IDLE = new THREE.Color(0xdcdcdc);
const COOL = new THREE.Color(0x4fb3ff);
const ALARM = new THREE.Color(0xff3b30);
const OPEN = new THREE.Color(0xffa52e);

/** Paints a marker for a state: off (unreachable), idle, on, lit (a lamp
 *  shining, `level` 0–1), alarm, open. */
export function paint(m, shape, state, level, glowColor) {
  const reachable = state !== 'off';
  m.body.color.set(reachable ? 0xffffff : 0x8a8a8a);
  m.body.transparent = !reachable;
  m.body.opacity = reachable ? 1 : 0.55;
  let color = reachable ? IDLE : OFF;
  let strength = 0;
  if (state === 'lit' || (state === 'on' && shape !== 'climate')) {
    color = new THREE.Color(glowColor);
    strength = state === 'lit' ? 0.6 + level : 0.9;
  } else if (state === 'on') {
    color = COOL;
    strength = 0.8;
  } else if (state === 'alarm') {
    color = ALARM;
    strength = 1.2;
  } else if (state === 'open') {
    color = OPEN;
    strength = 0.9;
  }
  m.glow.color.copy(color);
  m.glow.emissive.copy(strength ? color : new THREE.Color(0x000000));
  m.glow.emissiveIntensity = strength;
}
