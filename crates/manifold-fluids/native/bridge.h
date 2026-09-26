#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ManifoldFluidsFrameStats {
    uint32_t particles;
    uint32_t triangles;
    uint32_t substeps;
    double simulation_ms;
    double meshing_ms;
} ManifoldFluidsFrameStats;

typedef struct ManifoldFluidsWhitewaterParticle {
    float position[3];
    float velocity[3];
    float lifetime;
    uint8_t type;
} ManifoldFluidsWhitewaterParticle;

int manifold_fluids_world_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                 double cell_size, uint32_t surface_subdivisions,
                                 int apic, void **world_out);
void manifold_fluids_world_destroy(void *world);
int manifold_fluids_world_add_fluid_box(void *world, const float *min, const float *max,
                                        const float *velocity);
int manifold_fluids_world_set_gravity(void *world, const float *gravity);
int manifold_fluids_world_set_surface_options(void *world, double marker_particle_scale,
                                               double smoothing, uint32_t smoothing_iterations);
int manifold_fluids_world_set_liquid_options(void *world, double viscosity,
                                              double surface_tension);
int manifold_fluids_world_set_time_step_options(void *world, uint32_t min_substeps,
                                                uint32_t max_substeps, uint32_t cfl,
                                                int adaptive_obstacles);
int manifold_fluids_world_set_whitewater_options(void *world, int enabled,
                                                 uint32_t max_particles, double wavecrest_rate,
                                                 double turbulence_rate, double min_energy,
                                                 double max_energy);
int manifold_fluids_world_set_emitter(void *world, const float *min, const float *max,
                                      const float *velocity, int enabled);
int manifold_fluids_world_set_obstacle(void *world, const float *previous_min,
                                       const float *previous_max, const float *current_min,
                                       const float *current_max, const float *next_min,
                                       const float *next_max);
int manifold_fluids_world_step(void *world, double dt, ManifoldFluidsFrameStats *stats_out);
int manifold_fluids_world_surface(void *world, const uint8_t **data_out, size_t *len_out);
int manifold_fluids_world_whitewater_count(void *world, size_t *count_out);
int manifold_fluids_world_whitewater(void *world, ManifoldFluidsWhitewaterParticle *particles,
                                     size_t capacity, size_t *count_out);
const char *manifold_fluids_last_error(void);

#ifdef __cplusplus
}
#endif
