/// from gh discussion: add is_billboard to standard material. Can we even see
///
///

// if its a mesh and a billboard - make rotation face view in vertex shader?

// need to adjust
// Need to sample color from billboard texture in fragment shader
//
//

type BillboardMaterial<M> = ExtendedMaterial<M, Billboard>;
