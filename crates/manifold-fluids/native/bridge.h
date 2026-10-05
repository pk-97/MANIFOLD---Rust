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
    uint32_t cap_hit;
    uint32_t numerical_recovery;
} ManifoldFluidsFrameStats;

typedef struct ManifoldFluidsWhitewaterParticle {
    float position[3];
    float velocity[3];
    float lifetime;
    uint8_t type;
} ManifoldFluidsWhitewaterParticle;

// Layout of manifold_fluids::ParticleRecord and the renderer's FluidParticle.
typedef struct ManifoldFluidsParticleRecord {
    float position_radius[4];
    float velocity[3];
    uint32_t id;
} ManifoldFluidsParticleRecord;

typedef struct ManifoldFluidsRigidBodyInput {
    float pose[7];
    float center[3];
    float linear_velocity[3];
    float angular_velocity[3];
    float external_linear_acceleration[3];
    float external_angular_acceleration[3];
    float inverse_mass;
    float inverse_inertia[9];
    uint32_t enabled;
} ManifoldFluidsRigidBodyInput;

typedef struct ManifoldFluidsRigidReaction {
    double linear[3];
    double angular[3];
    double delta_linear[3];
    double delta_angular[3];
} ManifoldFluidsRigidReaction;

int manifold_fluids_world_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                 double cell_size, uint32_t surface_subdivisions,
                                 int apic, uint64_t seed, void **world_out);
void manifold_fluids_world_destroy(void *world);
int manifold_fluids_world_add_fluid_box(void *world, const float *min, const float *max,
                                        const float *velocity);
int manifold_fluids_world_add_mesh(void *world, uint32_t slot, uint8_t role,
                                    const float *vertices, size_t vertex_count,
                                    const uint32_t *triangles, size_t triangle_count,
                                    const float *pose);
int manifold_fluids_world_add_fluid_mesh(void *world, const float *vertices,
                                         size_t vertex_count, const uint32_t *triangles,
                                         size_t triangle_count, const float *pose,
                                         const float *velocity);
int manifold_fluids_world_prepare_rigid_coupling(void *world, const uint32_t *slots,
                                                  const uint32_t *body_indices,
                                                  size_t collider_count, size_t body_count,
                                                  double density);
int manifold_fluids_world_set_rigid_bodies(void *world,
                                            const ManifoldFluidsRigidBodyInput *inputs,
                                            size_t count);
int manifold_fluids_world_rigid_reactions(void *world, ManifoldFluidsRigidReaction *out,
                                          size_t capacity, size_t *count_out);
int manifold_fluids_world_set_mesh_motion(void *world, uint32_t slot,
                                          const float *previous, const float *current,
                                          const float *next);
int manifold_fluids_world_set_mesh_enabled(void *world, uint32_t slot, int enabled);
int manifold_fluids_world_set_inflow_options(void *world, uint32_t slot,
                                             const float *velocity, float inherit_motion);
int manifold_fluids_world_set_collider_friction(void *world, uint32_t slot, float friction);
int manifold_fluids_world_remove_mesh(void *world, uint32_t slot);
int manifold_fluids_world_set_boundary_collisions(void *world, const int32_t *collisions,
                                                  size_t count);
int manifold_fluids_world_set_gravity(void *world, const float *gravity);
int manifold_fluids_world_set_force_fields(void *world, const float *values, size_t value_count,
                                            uint32_t width, uint32_t height, uint32_t depth,
                                            int enabled);
int manifold_fluids_world_set_surface_options(void *world, double marker_particle_scale,
                                               double smoothing, uint32_t smoothing_iterations);
int manifold_fluids_world_set_surface_reconstruction(void *world, int enabled);
int manifold_fluids_world_set_liquid_options(void *world, double viscosity,
                                              double surface_tension);
int manifold_fluids_world_set_time_step_options(void *world, uint32_t min_substeps,
                                                uint32_t max_substeps, uint32_t cfl,
                                                int adaptive_obstacles);
// Marker speed removal measures each frame against at most dt seconds; 0
// measures the whole frame.
int manifold_fluids_world_set_marker_speed_limit_interval(void *world, double dt);
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
int manifold_fluids_world_clear_obstacle(void *world);
int manifold_fluids_world_step(void *world, double dt, ManifoldFluidsFrameStats *stats_out);
int manifold_fluids_world_step_live(void *world, double dt, ManifoldFluidsFrameStats *stats_out);
int manifold_fluids_world_begin_frame(void *world, double dt);
int manifold_fluids_world_begin_live_frame(void *world, double dt);
int manifold_fluids_world_next_substep(void *world, double *dt_out);
int manifold_fluids_world_advance_substep(void *world, double dt);
int manifold_fluids_world_finish_frame(void *world, ManifoldFluidsFrameStats *stats_out);
void manifold_fluids_world_abort_frame(void *world);
int manifold_fluids_world_marker_motion(void *world, float *position_out, float *velocity_out);
int manifold_fluids_world_rest_waterline(void *world, uint32_t i, uint32_t k, double *height_out);
int manifold_fluids_world_surface(void *world, const uint8_t **data_out, size_t *len_out);
int manifold_fluids_world_capture_surface_frame(void *world, void **frame_out);
int manifold_fluids_world_capture_particle_frame(void *world, const float *offset,
                                                 ManifoldFluidsParticleRecord *particles,
                                                 size_t particle_capacity, float *solid,
                                                 size_t solid_capacity, size_t *count_out,
                                                 uint32_t *nodes_out, int32_t *fits_out);
int manifold_fluids_surface_frame_solid(void *frame, float *solid, size_t capacity,
                                        uint32_t *nodes_out);
void manifold_fluids_surface_frame_destroy(void *frame);
int manifold_fluids_surface_frame_mesh(void *frame, uint32_t subdivisions,
                                     double particle_scale, double smoothing,
                                     uint32_t iterations, double isolated_scale,
                                     const uint8_t **data_out,
                                     size_t *len_out);
int manifold_fluids_world_whitewater_count(void *world, size_t *count_out);
int manifold_fluids_world_whitewater(void *world, ManifoldFluidsWhitewaterParticle *particles,
                                     size_t capacity, size_t *count_out);
const char *manifold_fluids_last_error(void);

// Layout of manifold_fluids::WhitewaterSpawn and the renderer's spawn records.
typedef struct ManifoldFluidsWhitewaterSpawn {
    float position_lifetime[4];
    float velocity[3];
    uint32_t kind;
} ManifoldFluidsWhitewaterSpawn;

// FLIP's whitewater lifecycle with emission off, fed fields and spawns from
// outside (GPU_WHITEWATER_DESIGN.md D1, section 3.4). The grid is isize·jsize·ksize
// cells of `cell_size` from `origin`, scene metres; particles cross the
// boundary in scene space.
int manifold_fluids_whitewater_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                      double cell_size, const float *origin,
                                      uint32_t capacity, uint64_t seed, void **lifecycle_out);
void manifold_fluids_whitewater_destroy(void *lifecycle);
int manifold_fluids_whitewater_clear(void *lifecycle, uint64_t seed);
// Faces in the seam layout over face_cells, placed face_offset cells into the
// grid; level at the cell centres; solid at the grid nodes; gravity in m/s².
int manifold_fluids_whitewater_set_fields(void *lifecycle, const float *face_u,
                                          const float *face_v, const float *face_w,
                                          const uint32_t *face_cells, const uint32_t *face_offset,
                                          const float *level, const float *solid,
                                          const float *gravity);
// Loads the records with lifetime > 0, stride-thinned to the room left.
int manifold_fluids_whitewater_load(void *lifecycle, const ManifoldFluidsWhitewaterSpawn *spawns,
                                    size_t count, uint32_t *loaded_out, uint32_t *thinned_out);
int manifold_fluids_whitewater_step(void *lifecycle, double dt);
int manifold_fluids_whitewater_count(void *lifecycle, size_t *count_out);
int manifold_fluids_whitewater_particles(void *lifecycle,
                                         ManifoldFluidsWhitewaterParticle *particles,
                                         size_t capacity, size_t *count_out);

#ifdef MANIFOLD_WHITEWATER_ORACLE
// FLIP's own curvature of a cell-centred level set `phi` (isize·jsize·ksize,
// x fastest, cell size dx): ParticleLevelSet::calculateCurvatureGrid. Writes
// the reinitialised field its validity rule reads and the extended curvature.
// Test oracle only (the whitewater-oracle cargo feature).
int manifold_fluids_oracle_curvature(const float *phi, uint32_t isize, uint32_t jsize,
                                     uint32_t ksize, double dx, float *surface_phi_out,
                                     float *curvature_out);
// FLIP's own sheet seeding (ParticleSheeter::generateSheetParticles), before
// the fill-rate draw: markers at `positions` (count × 3 floats, grid-local,
// inside the grid), `phi` the cell-centred surface level set, x fastest.
// Writes up to `capacity` seeds and always the true count. Test oracle only.
// The process-wide FLIP thread count. Test oracle only.
int manifold_fluids_oracle_thread_count(int *count_out);
int manifold_fluids_oracle_sheet_particles(const float *positions, size_t count,
                                           const float *phi, uint32_t isize, uint32_t jsize,
                                           uint32_t ksize, double dx, float fill_threshold,
                                           float *seeds_out, size_t capacity,
                                           size_t *seed_count_out);
// FLIP's own emitter, then one update, on a lifecycle's last fields: markers
// at `positions` (count × 3 floats, scene metres), `curvature` at the cell
// centres, turbulence emission 0, lifetime variance 0.
int manifold_fluids_oracle_emit(void *lifecycle, const float *curvature, const float *positions,
                                size_t count, double dt);
#endif

#ifdef __cplusplus
}
#endif
