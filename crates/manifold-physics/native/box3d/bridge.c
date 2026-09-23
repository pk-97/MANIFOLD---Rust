#include "box3d/box3d.h"

#include <stdint.h>
#include <stddef.h>

_Static_assert( sizeof( b3Vec3 ) == sizeof( float ) * 3, "unexpected b3Vec3 layout" );
_Static_assert( sizeof( b3Quat ) == sizeof( float ) * 4, "unexpected b3Quat layout" );

enum
{
	BOX3D_BRIDGE_OK = 0,
	BOX3D_BRIDGE_ERROR = 1,
	BOX3D_BRIDGE_NO_SHAPE = 2,
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
}

uint32_t manifold_box3d_world_create( float gx, float gy, float gz )
{
	b3WorldDef definition = b3DefaultWorldDef();
	definition.gravity = (b3Vec3){ gx, gy, gz };
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

void manifold_box3d_world_step( uint32_t world_id, float dt, uint32_t substeps )
{
	b3World_Step( b3LoadWorldId( world_id ), dt, (int)substeps );
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
	b3BodyType body_type = box3d_body_type( kind );
	if ( body_type == b3_bodyTypeCount || hull_out == NULL )
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

	b3Vec3 local_points[255];
	for ( int i = 0; i < point_count; ++i )
	{
		local_points[i] = (b3Vec3){ points[3 * i], points[3 * i + 1], points[3 * i + 2] };
	}

	b3HullData* hull = b3CreateHull( local_points, point_count, point_count );
	if ( hull == NULL )
	{
		b3DestroyBody( body_id );
		return 0;
	}

	b3ShapeDef shape_definition = b3DefaultShapeDef();
	shape_definition.density = 1.0f;
	shape_definition.baseMaterial.friction = friction;
	shape_definition.baseMaterial.restitution = restitution;
	b3ShapeId shape_id = b3CreateHullShape( body_id, &shape_definition, hull );
	if ( shape_id.index1 == 0 )
	{
		b3DestroyHull( hull );
		b3DestroyBody( body_id );
		return 0;
	}

	box3d_set_mass( body_id, mass );
	*hull_out = (uintptr_t)hull;
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

	if ( b3Body_GetType( body_id ) != body_type )
	{
		b3Body_SetType( body_id, body_type );
	}

	b3ShapeId shape_ids[1];
	if ( b3Body_GetShapes( body_id, shape_ids, 1 ) != 1 )
	{
		return BOX3D_BRIDGE_NO_SHAPE;
	}
	b3Shape_SetFriction( shape_ids[0], friction );
	b3Shape_SetRestitution( shape_ids[0], restitution );
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

void manifold_box3d_destroy_hull( uintptr_t hull_value )
{
	if ( hull_value != 0 )
	{
		b3DestroyHull( (b3HullData*)hull_value );
	}
}
