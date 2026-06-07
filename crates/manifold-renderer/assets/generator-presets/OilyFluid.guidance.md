# Oily Fluid

*A self-feeding fluid simulation that marbles the image into slow, drifting, oil-on-water patterns.*

## How it works

Oily Fluid is a generator: it starts from black and makes its own image with no input. Everything grows from one source, a little noise added each frame, which a fluid simulation stirs into drifting marbled patterns. It keeps two images in memory between frames: the color (the picture, which is all the past noise stirred together) and a flow field (a vector field, an arrow at every pixel giving the direction and speed of motion). Both start black.

Each frame runs in four phases.

### Phase 1: Make the seed

Inject Noise generates a soft, cloud-like texture (simplex noise, a smooth gradient noise like Perlin, not harsh white-noise static). It is a moving slice through a 3D noise field, so it drifts and never quite repeats. This is the only new content the simulation ever gets. On a black start it is the whole image, and it keeps trickling in so the patterns never die out.

![The noise seed at full amplitude, before it is scaled for injection](preview://node:noise_combine)

*Shown at full amplitude. In the graph this pattern is scaled down to the Noise value (around 0.01) before being added to the color each frame. That looks like almost nothing on its own, but because Feedback keeps about 99.99% of the color, the whisper accumulates over many frames into the full image.*

### Phase 2: Build the flow

This decides how the seed gets pushed.

**Curl Forcing** finds where the stored color changes fastest, its edges (the gradient, by central differences), giving an arrow across each edge. It normalizes those to pure direction and rotates them 90°. A gradient turned 90° becomes a swirl (curl, a flow's rotation or vorticity), which makes the liquid circle shapes instead of spreading outward.

![Curl Forcing](preview://Curl Forcing)

**Smooth Velocity** softens the stored flow, shrinking it (box-filter downsample) then blurring it (separable Gaussian). A sharp flow tears the color into grain. A soft one moves it in slow thick sheets, the oily heaviness.

![Smooth Velocity](preview://Smooth Velocity)

**Advect Velocity** builds the new flow: the old flow drags itself forward (self-advection), eases off so it cannot blow up (damping), and adds the curl swirls. It is stored and used in Phase 3.

![Advect Velocity](preview://Advect Velocity)

### Phase 3: Stir the color

Advect Color drags the stored color along the new flow (advection, the core semi-Lagrangian transport step), fades it slightly so old material dies off (decay, set by Feedback), and mixes in the new noise. The result is what you see, and the stored color the next frame reads. That drag-fade-refill cycle, every frame, is the marbling.

![Advect Color](preview://Advect Color)

### Phase 4: Show it

Render Modes lights the flat color: it reads brightness as height, finds the surface tilt (normals, a normal map), and shines light across it for wet-looking relief. The Mode control picks the style:

- **Oil Slick** splits colors along the flow (flow-driven chromatic shift) for the petrol sheen.
- **Flow Field** and **Lines** smear a texture along the current (line integral convolution, LIC).
- **Height Map** lights the relief plainly (Lambertian diffuse).
- **PBR** makes it glossy (matcap base, Fresnel rim, Blinn specular).

![Render Modes](preview://Render Modes)

Then Color Grade applies the final brightness, hue, and contrast.

![Color Grade](preview://Color Grade)

## Controls

### Simulation

| Control | What it does |
|---|---|
| Speed | Overall flow rate. Scales how fast the flow and color move each frame. |
| Feedback | How long color lingers before fading. Higher gives longer trails and denser buildup. |
| Curl | Swirl strength. How hard the edges twist the flow into spirals. |
| Noise | How much fresh texture is injected each frame. |
| Velocity Damp | How quickly the flow loses energy. Lower settles it sooner. |
| Velocity Displace | How far the flow drags itself each step. |
| Color Displace | How far the color is dragged along the flow each step. |

### Look

| Control | What it does |
|---|---|
| Mode | Final look: Oil Slick, Flow Field, Height Map, PBR, or Lines. |
| Relief | Strength of the lit surface relief. |
| Chroma | Color-split amount in Oil Slick mode. |
| Brightness, Saturation, Hue, Contrast | Final color grade on the output. |

## Result

![Output](preview://Output)

*The final composited image after warm-up. The flow field develops within a second or two, while the color amplitude and surface relief build over the following seconds, so a fresh start looks flat before the marbling deepens.*
