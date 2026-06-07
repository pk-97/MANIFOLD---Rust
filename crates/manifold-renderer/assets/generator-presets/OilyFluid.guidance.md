# Oily Fluid

*A self-feeding fluid simulation that marbles the image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid is a generator. It takes no input and produces its own image, starting from a black frame and developing the picture over time.

The simulation maintains two buffers and updates them every frame:

- the **color**, the visible image, and
- the **flow**, a velocity field that stores a direction and speed for every pixel.

Both begin black, with no image and no motion. Each frame runs four phases in order. The phases are easiest to follow starting from the first frame, where both buffers are empty, and then watching the image accumulate.

### Reading the images

[[preview-legend]]

### Phase 1: Make the seed

Inject Noise produces a smooth, organic pattern (simplex noise, a gradient noise related to Perlin noise rather than uncorrelated static). The pattern evolves slowly because the node samples a moving plane through a 3D noise volume. The result is then scaled by the Noise parameter, roughly 0.01, so the amount passed on each frame is very small.

On the first frame the buffers are black, so this scaled pattern is the only signal present. It is the source of all image content, and it is added to the color in Phase 3.

![The noise pattern at full amplitude, before it is scaled for injection](preview://node:noise_combine)

*Shown at full amplitude. In the graph the pattern is scaled to roughly 0.01 before use. The image is built from many of these small contributions accumulated over time, not from one strong noise field.*

### Phase 2: Build the flow

Phase 2 derives the velocity field that will move the color this frame. It reads **the current color**, which is black on the first frame and the image accumulated so far on every later frame.

- **Curl Forcing** computes the gradient of the color, the direction of steepest change at each pixel, which points across the boundaries between regions. It rotates that gradient by 90 degrees to produce a field aligned with those boundaries. Applied as a force, a field aligned with edges drives the fluid to circulate around regions rather than flow across them. This rotational forcing is the curl the group is named for. With no gradient on a black frame there is no force. Once the color has structure, its boundaries set where the fluid turns.

![Curl Forcing](preview://Curl Forcing)

- **Smooth Velocity** downsamples the existing velocity field and applies a separable Gaussian blur. A high-frequency field would fragment the color into grain. A smooth, low-frequency field transports it in broad coherent sheets, which produces the thick, viscous appearance.

![Smooth Velocity](preview://Smooth Velocity)

- **Advect Velocity** assembles the velocity for this frame. The existing field is advected along itself, carrying its own momentum forward, then attenuated so it cannot grow without bound (set by Velocity Damp), and the curl force from Curl Forcing is added.

![Advect Velocity](preview://Advect Velocity)

### Phase 3: Stir the color

Phase 3 updates the visible image. Advect Color reads **the current color**, advects each pixel of it along the **velocity field from Phase 2**, attenuates the result, and adds the **scaled noise from Phase 1**.

The attenuation retains almost all of the existing color, controlled by Feedback at roughly 0.9999. Because so little is lost per frame, each new contribution is laid over everything retained so far. On the first frame there is nothing to advect, so the output is a single faint layer of noise. Over many frames the retained layers accumulate while advection transports and folds them, which is what forms the marbling.

![Advect Color](preview://Advect Color)

The color produced here is the updated image. It is passed to Phase 4 for display and written back as the buffer that the next frame reads in Phase 2.

### Phase 4: Show it

Phase 4 shades the color for display and does not feed back into the simulation. Render Modes interprets the color's brightness as a height field, derives surface normals from it (a normal map), and lights those normals, which gives the image its raised, wet appearance. The Mode control selects the shading style:

- **Oil Slick** offsets the color channels along the flow direction for a chromatic, petrol-like sheen (a flow-driven chromatic shift).
- **Flow Field** and **Lines** convolve a texture along the velocity field to visualize it (line integral convolution, LIC).
- **Height Map** applies plain directional lighting (Lambertian diffuse).
- **PBR** applies a glossy material (matcap base with a Fresnel rim and Blinn specular highlight).

![Render Modes](preview://Render Modes)

Color Grade then applies the final brightness, hue, and contrast, producing the frame on screen.

![Color Grade](preview://Color Grade)

### The image, frame by frame

On frame 1 both buffers are black. Phase 1 contributes a faint layer of noise, and Phases 2 and 3 have no structure to act on, so the output is nearly black. On frame 2 that faint layer provides the structure Phase 2 needs. Curl Forcing derives a small velocity field from its edges, and Phase 3 advects the layer along that field and adds another contribution. Because each frame retains almost all of the previous color, the contributions accumulate, advection folds them into marbling, and the overall brightness rises. After a few seconds the image is fully developed. The current color that Phase 2 reads is simply the output Phase 3 produced on the previous frame, and that chain terminates at the initial black frame.

![Output](preview://Output)

*The final composited image after warm-up. The velocity field develops within a second or two, while the color amplitude and surface relief build over the following several seconds, so a fresh start appears flat before the marbling develops.*

## Controls

### Simulation

| Control | What it does |
|---|---|
| Speed | Overall flow rate. Scales how fast the flow and color move each frame. |
| Feedback | How long color is retained before fading. Higher values give longer trails and denser buildup. |
| Curl | Swirl strength. How strongly edges rotate the flow into spirals. |
| Noise | How much new texture is injected each frame. |
| Velocity Damp | How quickly the flow loses energy. Lower values settle it sooner. |
| Velocity Displace | How far the flow advects itself each step. |
| Color Displace | How far the color is advected along the flow each step. |

### Look

| Control | What it does |
|---|---|
| Mode | Final look: Oil Slick, Flow Field, Height Map, PBR, or Lines. |
| Relief | Strength of the lit surface relief. |
| Chroma | Channel-offset amount in Oil Slick mode. |
| Brightness, Saturation, Hue, Contrast | Final color grade on the output. |
