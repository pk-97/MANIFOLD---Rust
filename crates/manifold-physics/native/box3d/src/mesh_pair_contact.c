// SPDX-License-Identifier: MIT
// Two-sided, exact triangle surface contacts. BVHs only reject distant pairs;
// the original triangles supply every distance and solver contact.
#include "contact.h"
#include "body.h"
#include "manifold.h"
#include "physics_world.h"
#include "shape.h"

typedef struct b3PairCluster
{
	struct b3PairCluster* next;
	b3Vec3 normal;
	b3LocalManifoldPoint points[5];
	int count;
} b3PairCluster;

typedef struct b3PairQuery
{
	const b3Mesh* a;
	const b3Mesh* b;
	b3Transform bToA;
	b3Vec3 centers;
	float margin;
	b3PairCluster* clusters;
	int clusterCount;
	b3Arena arena;
} b3PairQuery;

static b3AABB b3PairBounds( const b3MeshNode* node, b3Vec3 scale )
{
	b3Vec3 a = b3Mul( node->lowerBound, scale );
	b3Vec3 b = b3Mul( node->upperBound, scale );
	return (b3AABB){ b3Min( a, b ), b3Max( a, b ) };
}

static void b3PairTriangles( b3PairQuery* query, int indexA, int indexB )
{
	b3Triangle a = b3GetMeshTriangle( query->a, indexA );
	b3Triangle b = b3GetMeshTriangle( query->b, indexB );
	b3DistanceInput input = {
		.proxyA = { a.vertices, 3, 0.0f },
		.proxyB = { b.vertices, 3, 0.0f },
		.transform = query->bToA,
		.useRadii = false,
	};
	b3SimplexCache cache = { 0 };
	b3DistanceOutput distance = b3ShapeDistance( &input, &cache, NULL, 0 );
	if ( distance.distance > query->margin )
	{
		return;
	}
	b3Vec3 normal = distance.normal;
	if ( distance.distance < 1.0e-6f )
	{
		// Zero-thickness surfaces have no penetration normal at intersection.
		// Use the triangle plane facing the other body's centre. The outer
		// stepping policy must keep motion below the speculative margin.
		normal = b3MakeNormalFromPoints( a.vertices[0], a.vertices[1], a.vertices[2] );
		if ( b3Dot( normal, query->centers ) < 0.0f )
		{
			normal = b3Neg( normal );
		}
	}
	b3PairCluster* cluster = query->clusters;
	while ( cluster != NULL && b3Dot( cluster->normal, normal ) < 0.995f )
	{
		cluster = cluster->next;
	}
	if ( cluster == NULL )
	{
		cluster = b3Bump( &query->arena, sizeof( b3PairCluster ) );
		*cluster = (b3PairCluster){ .next = query->clusters, .normal = normal };
		query->clusters = cluster;
		query->clusterCount += 1;
	}
	b3LocalManifoldPoint point = { 0 };
	point.point = b3MulSV( 0.5f, b3Add( distance.pointA, distance.pointB ) );
	point.separation = distance.distance - B3_MESH_REST_OFFSET;
	point.triangleIndex = indexA;
	// Together these two full triangle indices identify the pair; no hash collisions.
	point.pair = (b3FeaturePair){ (uint8_t)( indexB >> 24 ), (uint8_t)( indexB >> 16 ),
								(uint8_t)( indexB >> 8 ), (uint8_t)indexB };
	cluster->points[cluster->count++] = point;
	if ( cluster->count > B3_MAX_MANIFOLD_POINTS )
	{
		cluster->count = b3ReduceMovingMeshPoints( cluster->points, cluster->count, cluster->normal );
	}
}

static void b3VisitMeshPair( b3PairQuery* query, const b3MeshNode* a, const b3MeshNode* b )
{
	b3AABB boundsA = b3PairBounds( a, query->a->scale );
	b3AABB boundsB = b3AABB_Transform( query->bToA, b3PairBounds( b, query->b->scale ) );
	b3Vec3 margin = { query->margin, query->margin, query->margin };
	b3AABB expanded = { b3Sub( boundsA.lowerBound, margin ), b3Add( boundsA.upperBound, margin ) };
	if ( b3AABB_Overlaps( expanded, boundsB ) == false )
	{
		return;
	}
	bool leafA = a->data.asLeaf.type == 3;
	bool leafB = b->data.asLeaf.type == 3;
	if ( leafA && leafB )
	{
		for ( uint32_t i = 0; i < a->data.asLeaf.triangleCount; ++i )
		{
			for ( uint32_t j = 0; j < b->data.asLeaf.triangleCount; ++j )
			{
				b3PairTriangles( query, a->triangleOffset + i, b->triangleOffset + j );
			}
		}
	}
	else if ( leafB || ( !leafA && b3LengthSquared( b3Sub( boundsA.upperBound, boundsA.lowerBound ) ) >=
								  b3LengthSquared( b3Sub( boundsB.upperBound, boundsB.lowerBound ) ) ) )
	{
		b3VisitMeshPair( query, a + 1, b );
		b3VisitMeshPair( query, a + a->data.asNode.childOffset, b );
	}
	else
	{
		b3VisitMeshPair( query, a, b + 1 );
		b3VisitMeshPair( query, a, b + b->data.asNode.childOffset );
	}
}

bool b3ComputeMeshPairManifolds( b3World* world, b3Contact* contact,
	const b3Shape* shapeA, b3WorldTransform xfA, const b3Shape* shapeB, b3WorldTransform xfB, b3Arena arena )
{
	b3BodySim* simA = b3GetBodySim( world, b3Array_Get( world->bodies, shapeA->bodyId ) );
	b3BodySim* simB = b3GetBodySim( world, b3Array_Get( world->bodies, shapeB->bodyId ) );
	b3BodyState* stateA = b3GetBodyState( world, b3Array_Get( world->bodies, shapeA->bodyId ) );
	b3BodyState* stateB = b3GetBodyState( world, b3Array_Get( world->bodies, shapeB->bodyId ) );
	b3Vec3 velocityA = stateA ? stateA->linearVelocity : b3Vec3_zero;
	b3Vec3 velocityB = stateB ? stateB->linearVelocity : b3Vec3_zero;
	float speed = b3Length( b3Sub( velocityB, velocityA ) );
	if ( stateA )
	{
		speed += b3Length( stateA->angularVelocity ) * b3Length( simA->maxExtent );
	}
	if ( stateB )
	{
		speed += b3Length( stateB->angularVelocity ) * b3Length( simB->maxExtent );
	}
	float dt = world->inv_dt > 0.0f ? 1.0f / world->inv_dt : 0.0f;
	// Rest clearance plus a bound on this step's surface motion. Closely packed
	// stationary scans need no 2 cm ring of speculative triangle pairs. Keep
	// the engine's full speculative distance whenever the motion requires it.
	float margin = b3MinFloat( B3_SPECULATIVE_DISTANCE,
		B3_MESH_REST_OFFSET + 0.25f * B3_LINEAR_SLOP + speed * dt + 2.0f * b3Length( world->gravity ) * dt * dt );
	b3PairQuery query = {
		.a = &shapeA->mesh, .b = &shapeB->mesh, .margin = margin,
		.bToA = b3InvMulWorldTransforms( xfA, xfB ),
		.centers = b3InvRotateVector( xfA.q, b3SubPos( simB->center, simA->center ) ),
		.arena = arena,
	};
	b3VisitMeshPair( &query, b3GetMeshNodes( shapeA->mesh.data ), b3GetMeshNodes( shapeB->mesh.data ) );
	arena = query.arena;
	int oldCount = contact->manifoldCount;
	b3Manifold* old = b3Bump( &arena, oldCount * sizeof( b3Manifold ) );
	if ( oldCount > 0 )
	{
		memcpy( old, contact->manifolds, oldCount * sizeof( b3Manifold ) );
	}
	if ( oldCount != query.clusterCount )
	{
		b3FreeManifolds( world, contact->manifolds, oldCount );
		contact->manifolds = b3AllocateManifolds( world, query.clusterCount );
		contact->manifoldCount = query.clusterCount;
	}
	else if ( oldCount > 0 )
	{
		memset( contact->manifolds, 0, oldCount * sizeof( b3Manifold ) );
	}

	b3Vec3 offset = b3SubPos( xfA.p, xfB.p );
	b3Vec3 centerA = b3RotateVector( xfA.q, simA->localCenter );
	int index = 0;
	for ( b3PairCluster* cluster = query.clusters; cluster; cluster = cluster->next )
	{
		b3Manifold* manifold = contact->manifolds + index++;
		*manifold = (b3Manifold){ 0 };
		manifold->normal = b3RotateVector( xfA.q, cluster->normal );
		manifold->pointCount = cluster->count;
		for ( int i = 0; i < cluster->count; ++i )
		{
			b3LocalManifoldPoint* source = cluster->points + i;
			b3ManifoldPoint* target = manifold->points + i;
			target->anchorA = b3RotateVector( xfA.q, source->point );
			target->anchorB = b3Add( target->anchorA, offset );
			target->separation = source->separation;
			target->triangleIndex = source->triangleIndex;
			target->featureId = b3MakeFeatureId( source->pair );
			for ( int j = 0; j < oldCount; ++j )
			{
				if ( b3Dot( old[j].normal, manifold->normal ) < 0.995f )
				{
					continue;
				}
				for ( int k = 0; k < old[j].pointCount; ++k )
				{
					b3ManifoldPoint* previous = old[j].points + k;
					if ( previous->triangleIndex == target->triangleIndex && previous->featureId == target->featureId &&
						 b3DistanceSquared( previous->anchorA, b3Sub( target->anchorA, centerA ) ) < B3_LINEAR_SLOP * B3_LINEAR_SLOP )
					{
						target->normalImpulse = previous->normalImpulse;
						target->persisted = true;
						previous->triangleIndex = -1;
					}
				}
			}
		}
	}
	const b3SurfaceMaterial* a = b3GetShapeMaterials( shapeA );
	const b3SurfaceMaterial* b = b3GetShapeMaterials( shapeB );
	contact->friction = world->frictionCallback( a->friction, a->userMaterialId, b->friction, b->userMaterialId );
	contact->restitution = world->restitutionCallback( a->restitution, a->userMaterialId, b->restitution, b->userMaterialId );
	contact->rollingResistance = 0.0f;
	contact->tangentVelocity = b3Sub( b3RotateVector( xfA.q, a->tangentVelocity ), b3RotateVector( xfB.q, b->tangentVelocity ) );
	return query.clusterCount > 0;
}
