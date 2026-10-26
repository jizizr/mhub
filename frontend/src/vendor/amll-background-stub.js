// AMLL core ships its lyric player and its WebGL/Pixi background renderers in
// one module. MHub uses the DOM lyric player and the WebGL mesh-gradient
// backdrop, so Vite aliases only the Pixi renderer's dependencies here; the
// matrix exports below are unused now that gl-matrix resolves normally.
class MatrixStub {
  transpose() { return this; }
}

class VectorStub {}

export const Mat4 = {
  fromValues: () => new MatrixStub(),
  clone: () => new MatrixStub(),
  create: () => new MatrixStub(),
  mul: () => new MatrixStub(),
};

export const Vec2 = { fromValues: () => new VectorStub(), create: () => new VectorStub() };
export const Vec3 = Vec2;
export const Vec4 = { ...Vec2, transformMat4: () => new VectorStub() };

function unavailable() {
  throw new Error("AMLL background renderers are not bundled in MHub");
}

export class Application { constructor() { unavailable(); } }
export class Container {}
export class Sprite {}
export class BlurFilter { constructor() { unavailable(); } }
export class BulgePinchFilter { constructor() { unavailable(); } }
export class ColorMatrixFilter { constructor() { unavailable(); } }
export const Texture = { from: unavailable };
