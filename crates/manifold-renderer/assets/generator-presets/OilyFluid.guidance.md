# Oily Fluid

*A self-feeding fluid simulation that marbles the image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid is a generator. It takes no input and produces its own image, starting from a black frame and developing the picture over time.

The simulation keeps two things in memory and updates them every frame:

- the **color**, the visible image, and
- the **flow**, which holds a direction and speed at every pixel: which way each part of the image is moving, and how fast.

Both begin black, with no image and no motion. Each frame runs four phases in order. The phases are easiest to follow starting from the first frame, where both are empty, and then watching the image accumulate.

### Reading the images

[[preview-legend]]

### Phase 1: Make the seed

Inject Noise produces a smooth, organic pattern (simplex noise, a gradient noise related to Perlin noise rather than uncorrelated static). The pattern evolves slowly because the node samples a moving plane through a 3D noise volume. The result is then scaled by the Noise parameter, roughly 0.01, so the amount passed on each frame is very small.

On the first frame the color and flow are both black, so this scaled pattern is the only signal present. It is the source of all image content, and it is added to the color in Phase 3.

![The noise pattern at full strength, before it is scaled for injection](preview://node:noise_combine)

*Shown at full strength. In the graph the pattern is scaled to roughly 0.01 before use. The image is built from many of these small contributions accumulated over time, not from one strong burst of noise.*

### Phase 2: Build the flow

Phase 2 builds the flow that will move the color this frame. It reads **the current color**, which is black on the first frame and the image accumulated so far on every later frame.

- **Curl Forcing** computes the gradient of the color: at each pixel, the direction in which the color changes fastest. That direction runs across the boundary between two regions, pointing from the darker side toward the lighter one. Rotating a vector by 90 degrees turns across into along, so the rotated gradient runs parallel to the boundary, following the contour of the region. A force directed along a region's contour pushes the fluid around the region instead of out through its edge, and that circulation is the curl the group is named for. On a black frame there is no gradient and therefore no force. Once the color has structure, the contours of its regions set where the fluid turns.

![Curl Forcing](preview://Curl Forcing)

- **Smooth Velocity** drops the flow to a lower resolution (downsamples it) and blurs it (a separable Gaussian blur). A sharp, detailed flow would fragment the color into grain. A smooth, broad flow transports it in coherent sheets, which produces the thick, viscous appearance.

![Smooth Velocity](preview://Smooth Velocity)

- **Advect Velocity** produces this frame's flow. It keeps the existing motion, which carries its momentum forward, reduces it a little each frame so it does not build up without limit (set by Velocity Damp), and adds the new swirls from Curl Forcing.

![Advect Velocity](preview://Advect Velocity)

### Phase 3: Stir the color

Phase 3 updates the visible image. Advect Color moves the current color along the **flow from Phase 2**. Moving an image along the flow this way is called advection: each pixel takes its color from the point just upstream that the flow would have carried to it, so the picture is dragged along the current, like ink on water. The same operation carried the flow's own motion forward in Phase 2, which is why that motion has momentum. Advect Color then fades the result and mixes in the **scaled noise from Phase 1**.

The fade retains almost all of the existing color, controlled by Feedback at roughly 0.9999. Because so little is lost per frame, each new contribution is laid over everything retained so far. On the first frame there is nothing to advect, so the output is a single faint layer of noise. Over many frames the retained layers accumulate while advection transports and folds them, which is what forms the marbling.

![Advect Color](preview://Advect Color)

The color produced here is the updated image. It is passed to Phase 4 for display and written back as the color that the next frame reads in Phase 2.

### Phase 4: Show it

Phase 4 shades the color for display and does not feed back into the simulation. Render Modes reads the color's brightness as a surface, with bright areas standing for high ground and dark for low. From that surface it builds a normal map, an image that records the direction the surface faces at each point, which is what lighting needs in order to shade it. Lighting the normal map gives the image its raised, wet appearance. The Mode control selects the shading style:

- **Oil Slick** offsets the color channels along the flow direction for a chromatic, petrol-like sheen (a flow-driven chromatic shift).
- **Flow Field** and **Lines** draw a texture into streaks that follow the flow, making it visible (line integral convolution, LIC).
- **Height Map** applies plain directional lighting (Lambertian diffuse).
- **PBR** applies a glossy material (matcap base with a Fresnel rim and Blinn specular highlight).

![Render Modes](preview://Render Modes)

Color Grade then applies the final brightness, hue, and contrast, producing the frame on screen.

![Color Grade](preview://Color Grade)

### The image, frame by frame

On frame 1 the color and flow are both black. Phase 1 contributes a faint layer of noise, and Phases 2 and 3 have no structure to act on, so the output is nearly black. On frame 2 that faint layer provides the structure Phase 2 needs. Curl Forcing derives a small flow from its edges, and Phase 3 advects the layer along that flow and adds another contribution. Because each frame retains almost all of the previous color, the contributions accumulate, advection folds them into marbling, and the overall brightness rises. After a few seconds the image is fully developed. The current color that Phase 2 reads is simply the output Phase 3 produced on the previous frame, and that chain terminates at the initial black frame.

![Output](preview://Output)

*The final composited image after warm-up. The flow develops within a second or two, while the color and surface relief build over the following several seconds, so a fresh start appears flat before the marbling develops.*

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
