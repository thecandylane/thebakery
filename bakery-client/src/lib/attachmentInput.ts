// What a composer attachment becomes before it is uploaded. Every file is
// accepted on every runtime (the node decides how each model reads it) and
// goes as picked; an image a model cannot take whole also goes fitted.

/** Largest file a composer uploads. The node takes 500 MiB, but the upload
 *  sends a file as one base64 string, and the app's engine holds a string of
 *  at most ~512 Mi characters: past ~380 MiB it fails. Honest until the node
 *  streams uploads. */
export const MAX_ATTACHMENT_BYTES = 380 * 1024 * 1024;

/** The long edge every provider accepts, even in a long conversation:
 *  Claude caps each image at 2000 px once a request carries more than 20. */
const MAX_EDGE = 2000;
/** Under every provider's per-image limit once base64-encoded. */
const MAX_IMAGE_BYTES = 3_500_000;
/** PNG and JPEG are the formats every provider reads (Grok takes no others). */
const PORTABLE = new Set(["image/png", "image/jpeg"]);

async function decode(blob: Blob): Promise<ImageBitmap | HTMLImageElement> {
  try {
    return await createImageBitmap(blob);
  } catch {
    // Some engines decode a format in <img> they cannot in createImageBitmap.
    const url = URL.createObjectURL(blob);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      return img;
    } finally {
      URL.revokeObjectURL(url);
    }
  }
}

function encode(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

/** An image every model can read: PNG or JPEG, at most 2000 px on its long
 *  edge and a few MB. A PNG or JPEG that already fits is returned as it is
 *  (`null`), so its bytes are never touched; anything else that decodes (a
 *  4K screenshot, HEIC, WebP, GIF, TIFF…) is redrawn — as PNG when that
 *  fits, keeping text crisp, else as JPEG. `null` also for a file that is
 *  not an image or does not decode: it is sent as it is, and the node names
 *  it to a model that cannot open it. */
export async function fitImageForModels(file: Blob, filename: string): Promise<{ blob: Blob; filename: string } | null> {
  const type = file.type.toLowerCase();
  if (!type.startsWith("image/") || type === "image/svg+xml") return null;
  let image: ImageBitmap | HTMLImageElement;
  try {
    image = await decode(file);
  } catch {
    return null;
  }
  const width = "naturalWidth" in image ? image.naturalWidth : image.width;
  const height = "naturalHeight" in image ? image.naturalHeight : image.height;
  if (!width || !height) return null;
  if (PORTABLE.has(type) && Math.max(width, height) <= MAX_EDGE && file.size <= MAX_IMAGE_BYTES) return null;

  const scale = Math.min(1, MAX_EDGE / Math.max(width, height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  const context = canvas.getContext("2d");
  if (!context) return null;
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  if ("close" in image) image.close();

  // Named apart from the original it is sent beside.
  const stem = `${filename.replace(/\.[^.]*$/, "") || "image"}-fitted`;
  const png = await encode(canvas, "image/png");
  if (png && png.size <= MAX_IMAGE_BYTES) return { blob: png, filename: `${stem}.png` };
  for (const quality of [0.85, 0.7, 0.55]) {
    const jpeg = await encode(canvas, "image/jpeg", quality);
    if (jpeg && jpeg.size <= MAX_IMAGE_BYTES) return { blob: jpeg, filename: `${stem}.jpg` };
  }
  return null;
}
