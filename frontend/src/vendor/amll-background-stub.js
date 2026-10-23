// AMLL core ships its lyric player and its WebGL/Pixi background renderers in
// one module. MHub only uses the DOM lyric player, so Vite aliases the
// background-only dependencies here. Nothing below is called at runtime except
// the two module-level matrix constants AMLL evaluates on import.
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
