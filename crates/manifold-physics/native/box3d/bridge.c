#include "box3d/box3d.h"

#include <stdint.h>
#include <stddef.h>
#include <math.h>

_Static_assert( sizeof( b3Vec3 ) == sizeof( float ) * 3, "unexpected b3Vec3 layout" );
_Static_assert( sizeof( b3Quat ) == sizeof( float ) * 4, "unexpected b3Quat layout" );

enum
{
	BOX3D_BRIDGE_OK = 0,
	BOX3D_BRIDGE_ERROR = 1,
	BOX3D_BRIDGE_NO_SHAPE = 2,
	BOX3D_BRIDGE_NO_HIT = 3,
	BOX3D_BRIDGE_TOO_MANY_SHAPES = 4,
	BOX3D_MAX_BODY_SHAPES = 64,
};

static b3BodyType box3d_body_type( int kind )
{
	switch ( kind )
	{
		case 0: return b3_staticBody;
		case 1: return b3_dynamicBody;
		case 2: return b3_kinematicBody;
		default: return b3_bodyTypeCount;
	}
}

static b3Quat box3d_quat( float x, float y, float z, float w )
{
	b3Quat result = { { x, y, z }, w };
	return result;
}

uintptr_t manifold_box3d_cook_hull( const float* points, int point_count, int max_vertex_count )
{
	if ( points == NULL || point_count < 4 || max_vertex_count < 4 )
	{
		return 0;
	}
	return (uintptr_t)b3CreateHull( (const b3Vec3*)points, point_count, max_vertex_count );
}

int manifold_box3d_hull_copy_points( uintptr_t hull_value, float* points_out, int capacity )
{
	if ( hull_value == 0 )
	{
		return 0;
	}
	b3HullData* hull = (b3HullData*)hull_value;
	int count = hull->vertexCount;
	if ( points_out == NULL || capacity < count )
	{
		return count;
	}
	const b3Vec3* points = b3GetHullPoints( hull );
	for ( int i = 0; i < count; ++i )
	{
		points_out[3 * i] = points[i].x;
		points_out[3 * i + 1] = points[i].y;
		points_out[3 * i + 2] = points[i].z;
	}
	return count;
}

int manifold_box3d_hull_copy_triangles( uintptr_t hull_value, uint32_t* triangles_out, int capacity )
{
	if ( hull_value == 0 || capacity < 0 )
	{
		return -1;
	}

	b3HullData* hull = (b3HullData*)hull_value;
	if ( hull->vertexCount < 4 || hull->edgeCount < 6 || hull->faceCount < 4 )
	{
		return -1;
	}
	const b3HullHalfEdge* edges = b3GetHullEdges( hull );
	const b3HullFace* faces = b3GetHullFaces( hull );
	if ( edges == NULL || faces == NULL )
	{
		return -1;
	}

	int triangle_count = 0;
	for ( int face_index = 0; face_index < hull->faceCount; ++face_index )
	{
		int start = faces[face_index].edge;
		if ( start < 0 || start >= hull->edgeCount )
		{
			return -1;
		}
		int edge_index = start;
		int vertex_count = 0;
		do
		{
			if ( vertex_count >= hull->edgeCount || edge_index < 0 || edge_index >= hull->edgeCount )
			{
				return -1;
			}
			const b3HullHalfEdge* edge = edges + edge_index;
			if ( edge->face != face_index || edge->origin >= hull->vertexCount || edge->next >= hull->edgeCount ||
				edge->twin >= hull->edgeCount || edges[edge->twin].twin != edge_index )
			{
				return -1;
			}
			++vertex_count;
			edge_index = edge->next;
		}
		while ( edge_index != start );

		if ( vertex_count < 3 || triangle_count > 2147483647 - ( vertex_count - 2 ) )
		{
			return -1;
		}
		triangle_count += vertex_count - 2;
	}

	if ( triangles_out == NULL || capacity < triangle_count )
	{
		return triangle_count;
	}

	int triangle_index = 0;
	for ( int face_index = 0; face_index < hull->faceCount; ++face_index )
	{
		int start = faces[face_index].edge;
		int previous = edges[start].next;
		uint8_t first_origin = edges[start].origin;
		int edge_index = edges[previous].next;
		while ( edge_index != start )
		{
			const b3HullHalfEdge* edge = edges + edge_index;
			triangles_out[3 * triangle_index + 0] = first_origin;
			triangles_out[3 * triangle_index + 1] = edges[previous].origin;
			triangles_out[3 * triangle_index + 2] = edge->origin;
			++triangle_index;
			previous = edge_index;
			edge_index = edge->next;
		}
	}
	return triangle_index;
}

static void box3d_set_mass( b3BodyId body_id, float mass )
{
	if ( mass <= 0.0f || b3Body_GetType( body_id ) != b3_dynamicBody )
	{
		return;
	}

	b3MassData data = b3Body_GetMassData( body_id );
	if ( data.mass > 0.0f )
	{
		float scale = mass / data.mass;
		data.inertia.cx.x *= scale;
		data.inertia.cx.y *= scale;
		data.inertia.cx.z *= scale;
		data.inertia.cy.x *= scale;
		data.inertia.cy.y *= scale;
		data.inertia.cy.z *= scale;
		data.inertia.cz.x *= scale;
		data.inertia.cz.y *= scale;
		data.inertia.cz.z *= scale;
	}
	data.mass = mass;
	b3Body_SetMassData( body_id, data );
	/* SetMassData refreshes local inverse inertia but leaves the cached world
	 * matrix stale until the native pose is refreshed. Preserve the pose while
	 * forcing that refresh at the bridge seam. */
	b3Body_SetTransform( body_id, b3Body_GetPosition( body_id ), b3Body_GetRotation( body_id ) );
}

/* A shape tagged as a generated domain wall carries this user material id. */
#define MANIFOLD_WALL_MATERIAL 1u

/* A wall takes the friction of whatever touches it, so a body's own friction
 * is its slide threshold on a domain floor. Two ordinary shapes, or two walls,
 * keep Box3D's geometric mean. */
static float manifold_friction( float friction_a, uint64_t material_a, float friction_b, uint64_t material_b )
{
	int wall_a = material_a == MANIFOLD_WALL_MATERIAL;
	int wall_b = material_b == MANIFOLD_WALL_MATERIAL;
	if ( wall_a && !wall_b )
	{
		return friction_b;
	}
	if ( wall_b && !wall_a )
	{
		return friction_a;
	}
	return sqrtf( friction_a * friction_b );
}

uint32_t manifold_box3d_world_create( float gx, float gy, float gz )
{
	b3WorldDef definition = b3DefaultWorldDef();
	definition.gravity = (b3Vec3){ gx, gy, gz };
	definition.frictionCallback = manifold_friction;
	definition.workerCount = 1;
	b3WorldId world_id = b3CreateWorld( &definition );
	return b3StoreWorldId( world_id );
}

void manifold_box3d_world_destroy( uint32_t world_id )
{
	b3DestroyWorld( b3LoadWorldId( world_id ) );
}

void manifold_box3d_world_set_gravity( uint32_t world_id, float gx, float gy, float gz )
{
	b3World_SetGravity( b3LoadWorldId( world_id ), (b3Vec3){ gx, gy, gz } );
}

void manifold_box3d_world_set_max_linear_speed( uint32_t world_id, float speed )
{
	b3World_SetMaximumLinearSpeed( b3LoadWorldId( world_id ), speed );
}

void manifold_box3d_world_set_contact_tuning( uint32_t world_id, float hertz, float damping, float speed )
{
	b3World_SetContactTuning( b3LoadWorldId( world_id ), hertz, damping, speed );
}

void manifold_box3d_world_step( uint32_t world_id, float dt, uint32_t substeps )
{
	b3World_Step( b3LoadWorldId( world_id ), dt, (int)substeps );
}

static uint64_t box3d_body_create_hulls(
	uint32_t world_id,
	const float* points,
	const int32_t* point_counts,
	int hull_count,
	int max_vertex_count,
	int kind,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float mass,
	float friction,
	float restitution,
	uintptr_t* hull_out )
{
	b3BodyType body_type = box3d_body_type( kind );
	if ( body_type == b3_bodyTypeCount || points == NULL || point_counts == NULL || hull_out == NULL || hull_count < 1 || max_vertex_count < 4 )
	{
		return 0;
	}

	b3BodyDef body_definition = b3DefaultBodyDef();
	body_definition.type = body_type;
	body_definition.position = (b3Pos){ px, py, pz };
	body_definition.rotation = box3d_quat( qx, qy, qz, qw );
	b3BodyId body_id = b3CreateBody( b3LoadWorldId( world_id ), &body_definition );
	if ( body_id.index1 == 0 )
	{
		return 0;
	}

	b3ShapeDef shape_definition = b3DefaultShapeDef();
	shape_definition.density = 1.0f;
	shape_definition.baseMaterial.friction = friction;
	shape_definition.baseMaterial.restitution = restitution;
	size_t point_offset = 0;
	for ( int i = 0; i < hull_count; ++i )
	{
		int point_count = point_counts[i];
		if ( point_count < 4 )
		{
			for ( int j = 0; j < i; ++j )
			{
				b3DestroyHull( (b3HullData*)hull_out[j] );
			}
			b3DestroyBody( body_id );
			return 0;
		}
		b3HullData* hull = b3CreateHull( (const b3Vec3*)( points + 3 * point_offset ), point_count, max_vertex_count );
		if ( hull == NULL )
		{
			for ( int j = 0; j < i; ++j )
			{
				b3DestroyHull( (b3HullData*)hull_out[j] );
			}
			b3DestroyBody( body_id );
			return 0;
		}
		b3ShapeId shape_id = b3CreateHullShape( body_id, &shape_definition, hull );
		if ( shape_id.index1 == 0 )
		{
			b3DestroyHull( hull );
			for ( int j = 0; j < i; ++j )
			{
				b3DestroyHull( (b3HullData*)hull_out[j] );
			}
			b3DestroyBody( body_id );
			return 0;
		}
		hull_out[i] = (uintptr_t)hull;
		point_offset += (size_t)point_count;
	}

	box3d_set_mass( body_id, mass );
	return b3StoreBodyId( body_id );
}

uint64_t manifold_box3d_body_create(
	uint32_t world_id,
	const float* points,
	int point_count,
	int kind,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float mass,
	float friction,
	float restitution,
	uintptr_t* hull_out )
{
	int32_t point_counts[1] = { point_count };
	return box3d_body_create_hulls( world_id, points, point_counts, 1, point_count, kind, px, py, pz, qx, qy, qz, qw,
		mass, friction, restitution, hull_out );
}

uint64_t manifold_box3d_body_create_hulls(
	uint32_t world_id,
	const float* points,
	const int32_t* point_counts,
	int hull_count,
	int max_vertex_count,
	int kind,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float mass,
	float friction,
	float restitution,
	uintptr_t* hull_out )
{
	return box3d_body_create_hulls( world_id, points, point_counts, hull_count, max_vertex_count, kind, px, py, pz, qx, qy, qz, qw,
		mass, friction, restitution, hull_out );
}

uint64_t manifold_box3d_mesh_body_create(
	uint32_t world_id,
	const float* vertices,
	int vertex_count,
	const int32_t* indices,
	int triangle_count,
	int kind,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float mass,
	float friction,
	float restitution,
	const float* center,
	const float* inertia,
	uintptr_t* mesh_out )
{
	b3BodyType body_type = box3d_body_type( kind );
	if ( body_type == b3_bodyTypeCount || vertices == NULL || indices == NULL || center == NULL || inertia == NULL || mesh_out == NULL ||
		vertex_count < 3 || triangle_count < 1 )
	{
		return 0;
	}

	b3BodyDef body_definition = b3DefaultBodyDef();
	body_definition.type = body_type;
	body_definition.position = (b3Pos){ px, py, pz };
	body_definition.rotation = box3d_quat( qx, qy, qz, qw );
	b3BodyId body_id = b3CreateBody( b3LoadWorldId( world_id ), &body_definition );
	if ( body_id.index1 == 0 )
	{
		return 0;
	}

	b3MeshDef mesh_definition = { 0 };
	mesh_definition.vertices = (b3Vec3*)vertices;
	mesh_definition.indices = (int32_t*)indices;
	mesh_definition.vertexCount = vertex_count;
	mesh_definition.triangleCount = triangle_count;
	mesh_definition.useMedianSplit = true;
	mesh_definition.identifyEdges = true;
	b3MeshData* mesh = b3CreateMesh( &mesh_definition, NULL, 0 );
	if ( mesh == NULL || mesh->triangleCount != triangle_count )
	{
		if ( mesh != NULL )
		{
			b3DestroyMesh( mesh );
		}
		b3DestroyBody( body_id );
		if ( mesh != NULL )
		{
			return UINT64_MAX;
		}
		return 0;
	}

	b3ShapeDef shape_definition = b3DefaultShapeDef();
	shape_definition.density = 0.0f;
	shape_definition.baseMaterial.friction = friction;
	shape_definition.baseMaterial.restitution = restitution;
	b3ShapeId shape_id = b3CreateMeshShape( body_id, &shape_definition, mesh, b3Vec3_one );
	if ( shape_id.index1 == 0 )
	{
		b3DestroyMesh( mesh );
		b3DestroyBody( body_id );
		return 0;
	}

	b3MassData mass_data = {
		.mass = mass,
		.center = { center[0], center[1], center[2] },
		.inertia = {
			{ inertia[0], inertia[1], inertia[2] },
			{ inertia[3], inertia[4], inertia[5] },
			{ inertia[6], inertia[7], inertia[8] },
		},
	};
	b3Body_SetMassData( body_id, mass_data );
	b3Body_SetTransform( body_id, b3Body_GetPosition( body_id ), b3Body_GetRotation( body_id ) );
	*mesh_out = (uintptr_t)mesh;
	return b3StoreBodyId( body_id );
}

int manifold_box3d_body_update(
	uint64_t body_value,
	int kind,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float mass,
	float friction,
	float restitution,
	int move_pose )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	b3BodyType body_type = box3d_body_type( kind );
	if ( body_type == b3_bodyTypeCount )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	int shape_count = b3Body_GetShapeCount( body_id );
	if ( shape_count < 1 )
	{
		return BOX3D_BRIDGE_NO_SHAPE;
	}
	if ( shape_count > BOX3D_MAX_BODY_SHAPES )
	{
		return BOX3D_BRIDGE_TOO_MANY_SHAPES;
	}

	// Box3D's bullet flag is meaningful only for dynamic bodies, but it is
	// stored on the body independently of its type. Clear it before changing a
	// dynamic body to static or kinematic so a later return to dynamic cannot
	// silently re-enable the old CCD state.
	if ( body_type != b3_dynamicBody && b3Body_GetType( body_id ) == b3_dynamicBody )
	{
		b3Body_SetBullet( body_id, false );
	}

	if ( b3Body_GetType( body_id ) != body_type )
	{
		b3Body_SetType( body_id, body_type );
	}

	b3ShapeId shape_ids[BOX3D_MAX_BODY_SHAPES];
	int stored_shape_count = b3Body_GetShapes( body_id, shape_ids, shape_count );
	if ( stored_shape_count != shape_count )
	{
		return BOX3D_BRIDGE_NO_SHAPE;
	}
	for ( int i = 0; i < shape_count; ++i )
	{
		b3Shape_SetFriction( shape_ids[i], friction );
		b3Shape_SetRestitution( shape_ids[i], restitution );
	}
	box3d_set_mass( body_id, mass );
	if ( move_pose != 0 )
	{
		b3Body_SetTransform( body_id, (b3Pos){ px, py, pz }, box3d_quat( qx, qy, qz, qw ) );
	}
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_bullet( uint64_t body_value, int enabled )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( b3Body_GetType( body_id ) != b3_dynamicBody )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Body_SetBullet( body_id, enabled != 0 );
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_wall( uint64_t body_value, int wall )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	b3ShapeId shape_ids[BOX3D_MAX_BODY_SHAPES];
	int shape_count = b3Body_GetShapes( body_id, shape_ids, BOX3D_MAX_BODY_SHAPES );
	if ( shape_count < 1 )
	{
		return BOX3D_BRIDGE_NO_SHAPE;
	}
	uint64_t tag = wall != 0 ? MANIFOLD_WALL_MATERIAL : 0u;
	int changed = 0;
	for ( int i = 0; i < shape_count; ++i )
	{
		if ( b3Shape_GetType( shape_ids[i] ) == b3_compoundShape )
		{
			return BOX3D_BRIDGE_ERROR;
		}
		b3SurfaceMaterial material = b3Shape_GetSurfaceMaterial( shape_ids[i] );
		if ( material.userMaterialId != tag )
		{
			material.userMaterialId = tag;
			b3Shape_SetSurfaceMaterial( shape_ids[i], material );
			changed = 1;
		}
	}
	/* A recycled contact never re-reads its materials (physics_world.c contact
	 * recycling), so a live retag must drop the body's contacts. Disabling
	 * destroys them; enabling lets the broadphase rebuild them with the new
	 * mixed friction. Re-enabling recreates the awake state at rest, so a
	 * moving body's velocities are carried across by hand. It also wakes the
	 * body and whatever touched it. */
	if ( changed && b3Body_IsEnabled( body_id ) )
	{
		int moving = b3Body_GetType( body_id ) != b3_staticBody;
		b3Vec3 linear = b3Body_GetLinearVelocity( body_id );
		b3Vec3 angular = b3Body_GetAngularVelocity( body_id );
		b3Body_Disable( body_id );
		b3Body_Enable( body_id );
		if ( moving )
		{
			b3Body_SetLinearVelocity( body_id, linear );
			b3Body_SetAngularVelocity( body_id, angular );
		}
	}
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_enabled( uint64_t body_value, int enabled )
{
	if ( enabled != 0 )
	{
		b3Body_Enable( b3LoadBodyId( body_value ) );
	}
	else
	{
		b3Body_Disable( b3LoadBodyId( body_value ) );
	}
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_hit_events( uint64_t body_value, int enabled )
{
	b3Body_EnableHitEvents( b3LoadBodyId( body_value ), enabled != 0 );
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_hit_speed( uint32_t world_value, uint64_t body_value, float* speed_out )
{
	if ( speed_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	b3ContactEvents events = b3World_GetContactEvents( b3LoadWorldId( world_value ) );
	float max_speed = 0.0f;
	bool found = false;
	for ( int i = 0; i < events.hitCount; ++i )
	{
		b3ContactHitEvent event = events.hitEvents[i];
		b3BodyId body_a = b3Shape_GetBody( event.shapeIdA );
		b3BodyId body_b = b3Shape_GetBody( event.shapeIdB );
		if ( B3_ID_EQUALS( body_a, body_id ) || B3_ID_EQUALS( body_b, body_id ) )
		{
			if ( !found || event.approachSpeed > max_speed )
			{
				max_speed = event.approachSpeed;
				found = true;
			}
		}
	}

	if ( !found )
	{
		return BOX3D_BRIDGE_NO_HIT;
	}
	*speed_out = max_speed;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_linear_velocity( uint64_t body_value, float* velocity_out )
{
	if ( velocity_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Vec3 velocity = b3Body_GetLinearVelocity( b3LoadBodyId( body_value ) );
	velocity_out[0] = velocity.x;
	velocity_out[1] = velocity.y;
	velocity_out[2] = velocity.z;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_angular_velocity( uint64_t body_value, float* velocity_out )
{
	if ( velocity_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Vec3 velocity = b3Body_GetAngularVelocity( b3LoadBodyId( body_value ) );
	velocity_out[0] = velocity.x;
	velocity_out[1] = velocity.y;
	velocity_out[2] = velocity.z;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_local_point_velocity(
	uint64_t body_value,
	float px,
	float py,
	float pz,
	float* velocity_out )
{
	if ( velocity_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Vec3 velocity = b3Body_GetLocalPointVelocity(
		b3LoadBodyId( body_value ), (b3Vec3){ px, py, pz } );
	velocity_out[0] = velocity.x;
	velocity_out[1] = velocity.y;
	velocity_out[2] = velocity.z;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_local_center_of_mass( uint64_t body_value, float* center_out )
{
	if ( center_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Vec3 center = b3Body_GetLocalCenterOfMass( b3LoadBodyId( body_value ) );
	center_out[0] = center.x;
	center_out[1] = center.y;
	center_out[2] = center.z;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_velocity(
	uint64_t body_value,
	float linear_x,
	float linear_y,
	float linear_z,
	float angular_x,
	float angular_y,
	float angular_z )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	b3Body_SetLinearVelocity( body_id, (b3Vec3){ linear_x, linear_y, linear_z } );
	b3Body_SetAngularVelocity( body_id, (b3Vec3){ angular_x, angular_y, angular_z } );
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_set_target(
	uint64_t body_value,
	float px,
	float py,
	float pz,
	float qx,
	float qy,
	float qz,
	float qw,
	float time_step )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( b3Body_GetType( body_id ) != b3_kinematicBody || time_step <= 0.0f )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3WorldTransform target = { (b3Pos){ px, py, pz }, box3d_quat( qx, qy, qz, qw ) };
	b3Body_SetTargetTransform( body_id, target, time_step, true );
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_pose( uint64_t body_value, float* position_out, float* rotation_out )
{
	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( position_out == NULL || rotation_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	b3Pos position = b3Body_GetPosition( body_id );
	b3Quat rotation = b3Body_GetRotation( body_id );
	position_out[0] = (float)position.x;
	position_out[1] = (float)position.y;
	position_out[2] = (float)position.z;
	rotation_out[0] = rotation.v.x;
	rotation_out[1] = rotation.v.y;
	rotation_out[2] = rotation.v.z;
	rotation_out[3] = rotation.s;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_field_state(
	uint64_t body_value,
	float* center_out,
	float* mass_out,
	int* type_out,
	int* enabled_out )
{
	if ( center_out == NULL || mass_out == NULL || type_out == NULL || enabled_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( !b3Body_IsValid( body_id ) )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3Pos center = b3Body_GetWorldCenterOfMass( body_id );
	center_out[0] = (float)center.x;
	center_out[1] = (float)center.y;
	center_out[2] = (float)center.z;
	*mass_out = b3Body_GetMass( body_id );
	switch ( b3Body_GetType( body_id ) )
	{
		case b3_staticBody: *type_out = 0; break;
		case b3_dynamicBody: *type_out = 1; break;
		case b3_kinematicBody: *type_out = 2; break;
		default: return BOX3D_BRIDGE_ERROR;
	}
	*enabled_out = b3Body_IsEnabled( body_id ) ? 1 : 0;
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_dynamics(
	uint64_t body_value,
	float* center_out,
	float* linear_out,
	float* angular_out,
	float* inverse_mass_out,
	float* inverse_inertia_out,
	int* type_out,
	int* enabled_out,
	int* awake_out,
	float* external_linear_out,
	float* external_angular_out )
{
	if ( center_out == NULL || linear_out == NULL || angular_out == NULL || inverse_mass_out == NULL ||
		inverse_inertia_out == NULL || type_out == NULL || enabled_out == NULL || awake_out == NULL ||
		external_linear_out == NULL || external_angular_out == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( !b3Body_IsValid( body_id ) )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3Pos center = b3Body_GetWorldCenterOfMass( body_id );
	b3Vec3 linear = b3Body_GetLinearVelocity( body_id );
	b3Vec3 angular = b3Body_GetAngularVelocity( body_id );
	b3Matrix3 inverse_inertia = b3Body_GetWorldInverseRotationalInertia( body_id );
	center_out[0] = (float)center.x;
	center_out[1] = (float)center.y;
	center_out[2] = (float)center.z;
	linear_out[0] = linear.x;
	linear_out[1] = linear.y;
	linear_out[2] = linear.z;
	angular_out[0] = angular.x;
	angular_out[1] = angular.y;
	angular_out[2] = angular.z;
	*inverse_mass_out = b3Body_GetInverseMass( body_id );
	/* b3Matrix3 stores its columns in cx/cy/cz; export rows explicitly. */
	inverse_inertia_out[0] = inverse_inertia.cx.x;
	inverse_inertia_out[1] = inverse_inertia.cy.x;
	inverse_inertia_out[2] = inverse_inertia.cz.x;
	inverse_inertia_out[3] = inverse_inertia.cx.y;
	inverse_inertia_out[4] = inverse_inertia.cy.y;
	inverse_inertia_out[5] = inverse_inertia.cz.y;
	inverse_inertia_out[6] = inverse_inertia.cx.z;
	inverse_inertia_out[7] = inverse_inertia.cy.z;
	inverse_inertia_out[8] = inverse_inertia.cz.z;
	switch ( b3Body_GetType( body_id ) )
	{
		case b3_staticBody: *type_out = 0; break;
		case b3_dynamicBody: *type_out = 1; break;
		case b3_kinematicBody: *type_out = 2; break;
		default: return BOX3D_BRIDGE_ERROR;
	}
	*enabled_out = b3Body_IsEnabled( body_id ) ? 1 : 0;
	*awake_out = b3Body_IsAwake( body_id ) ? 1 : 0;
	b3Vec3 external_linear;
	b3Vec3 external_angular;
	b3Body_GetExternalAccelerations( body_id, &external_linear, &external_angular );
	external_linear_out[0] = external_linear.x;
	external_linear_out[1] = external_linear.y;
	external_linear_out[2] = external_linear.z;
	external_angular_out[0] = external_angular.x;
	external_angular_out[1] = external_angular.y;
	external_angular_out[2] = external_angular.z;
	if ( !isfinite( external_linear.x ) || !isfinite( external_linear.y ) ||
		!isfinite( external_linear.z ) || !isfinite( external_angular.x ) ||
		!isfinite( external_angular.y ) || !isfinite( external_angular.z ) )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	return BOX3D_BRIDGE_OK;
}

static int box3d_vec3_finite( b3Vec3 value )
{
	return isfinite( value.x ) && isfinite( value.y ) && isfinite( value.z );
}

static int box3d_matrix_finite( b3Matrix3 value )
{
	return box3d_vec3_finite( value.cx ) && box3d_vec3_finite( value.cy ) && box3d_vec3_finite( value.cz );
}

int manifold_box3d_body_preflight_impulse(
	uint32_t world_value,
	uint64_t body_value,
	const float* linear,
	const float* angular )
{
	if ( linear == NULL || angular == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	for ( int i = 0; i < 3; ++i )
	{
		if ( !isfinite( linear[i] ) || !isfinite( angular[i] ) )
		{
			return BOX3D_BRIDGE_ERROR;
		}
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( !b3Body_IsValid( body_id ) || b3Body_GetType( body_id ) != b3_dynamicBody || !b3Body_IsEnabled( body_id ) )
	{
		return BOX3D_BRIDGE_OK;
	}

	if ( linear[0] != 0.0f || linear[1] != 0.0f || linear[2] != 0.0f )
	{
		b3Vec3 velocity = b3Body_GetLinearVelocity( body_id );
		float inverse_mass = b3Body_GetInverseMass( body_id );
		b3Vec3 predicted = b3MulAdd( velocity, inverse_mass, (b3Vec3){ linear[0], linear[1], linear[2] } );
		if ( !box3d_vec3_finite( velocity ) || !isfinite( inverse_mass ) || !box3d_vec3_finite( predicted ) )
		{
			return BOX3D_BRIDGE_ERROR;
		}
		float length_squared = b3LengthSquared( predicted );
		float maximum_speed = b3World_GetMaximumLinearSpeed( b3LoadWorldId( world_value ) );
		float maximum_speed_squared = maximum_speed * maximum_speed;
		if ( !isfinite( length_squared ) || !isfinite( maximum_speed ) || !isfinite( maximum_speed_squared ) ||
			length_squared > maximum_speed_squared )
		{
			return BOX3D_BRIDGE_ERROR;
		}
	}

	if ( angular[0] != 0.0f || angular[1] != 0.0f || angular[2] != 0.0f )
	{
		b3Vec3 velocity = b3Body_GetAngularVelocity( body_id );
		b3Quat rotation = b3Body_GetRotation( body_id );
		b3Vec3 impulse = { angular[0], angular[1], angular[2] };
		b3Vec3 local_impulse = b3InvRotateVector( rotation, impulse );
		b3Matrix3 local_inertia = b3Body_GetLocalRotationalInertia( body_id );
		b3Matrix3 local_inverse = b3Det( local_inertia ) > 0.0f ? b3InvertT( local_inertia ) : b3Mat3_zero;
		b3Vec3 local_delta = b3MulMV( local_inverse, local_impulse );
		b3Vec3 delta = b3RotateVector( rotation, local_delta );
		b3Vec3 predicted = b3Add( velocity, delta );
		if ( !box3d_vec3_finite( velocity ) || !box3d_vec3_finite( local_impulse ) ||
			!box3d_matrix_finite( local_inertia ) || !box3d_matrix_finite( local_inverse ) ||
			!box3d_vec3_finite( local_delta ) || !box3d_vec3_finite( delta ) || !box3d_vec3_finite( predicted ) )
		{
			return BOX3D_BRIDGE_ERROR;
		}
	}
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_apply_impulse(
	uint64_t body_value,
	const float* linear,
	const float* angular )
{
	if ( linear == NULL || angular == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	for ( int i = 0; i < 3; ++i )
	{
		if ( !isfinite( linear[i] ) || !isfinite( angular[i] ) )
		{
			return BOX3D_BRIDGE_ERROR;
		}
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( !b3Body_IsValid( body_id ) || b3Body_GetType( body_id ) != b3_dynamicBody || !b3Body_IsEnabled( body_id ) )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	int wake = ( linear[0] != 0.0f || linear[1] != 0.0f || linear[2] != 0.0f ||
		angular[0] != 0.0f || angular[1] != 0.0f || angular[2] != 0.0f );
	if ( linear[0] != 0.0f || linear[1] != 0.0f || linear[2] != 0.0f )
	{
		b3Body_ApplyLinearImpulseToCenter( body_id, (b3Vec3){ linear[0], linear[1], linear[2] }, wake != 0 );
	}
	if ( angular[0] != 0.0f || angular[1] != 0.0f || angular[2] != 0.0f )
	{
		b3Body_ApplyAngularImpulse( body_id, (b3Vec3){ angular[0], angular[1], angular[2] }, wake != 0 );
	}
	return BOX3D_BRIDGE_OK;
}

int manifold_box3d_body_apply_field(
	uint64_t body_value,
	const float* force,
	const float* impulse )
{
	if ( force == NULL || impulse == NULL )
	{
		return BOX3D_BRIDGE_ERROR;
	}
	for ( int i = 0; i < 3; ++i )
	{
		if ( !isfinite( force[i] ) || !isfinite( impulse[i] ) )
		{
			return BOX3D_BRIDGE_ERROR;
		}
	}

	b3BodyId body_id = b3LoadBodyId( body_value );
	if ( !b3Body_IsValid( body_id ) || b3Body_GetType( body_id ) != b3_dynamicBody )
	{
		return BOX3D_BRIDGE_ERROR;
	}

	b3Vec3 force_value = { force[0], force[1], force[2] };
	if ( force[0] != 0.0f || force[1] != 0.0f || force[2] != 0.0f )
	{
		b3Body_ApplyForceToCenter( body_id, force_value, true );
	}

	b3Vec3 impulse_value = { impulse[0], impulse[1], impulse[2] };
	if ( impulse[0] != 0.0f || impulse[1] != 0.0f || impulse[2] != 0.0f )
	{
		b3Body_ApplyLinearImpulseToCenter( body_id, impulse_value, true );
	}
	return BOX3D_BRIDGE_OK;
}

void manifold_box3d_destroy_hull( uintptr_t hull_value )
{
	if ( hull_value != 0 )
	{
		b3DestroyHull( (b3HullData*)hull_value );
	}
}

void manifold_box3d_destroy_mesh( uintptr_t mesh_value )
{
	if ( mesh_value != 0 )
	{
		b3DestroyMesh( (b3MeshData*)mesh_value );
	}
}
