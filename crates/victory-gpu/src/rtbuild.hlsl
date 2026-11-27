// Host ray-tracing structures from the title's own (crate::rtbuild): a
// structure's bytes, copied out of title memory, are walked from its root
// and what a host build needs is written: a bottom level's triangles, or a
// top level's instance descriptions. Node formats: crates/victory-shader
// src/bvh.rs.
//
// gather:  copies `words` dwords from u0 to u1.
// extract: one group per structure. u0 the structure's bytes (from its
//          header on), u1 what it writes, u2 a queue of box nodes (a count, then `queued` entries), u3 the
//          statistics (32 dwords at `slot`), t0 the host bottom levels by
//          title address (top levels): `entries` of (address >> 8, host
//          address low, high), sorted.
//
// A bottom level's triangle k of the triangle node at node index n goes to
// triangle 4n + k of u1 (36 bytes: the vertex neither barycentric weighs,
// then I's, then J's), so a host hit's primitive index names the title's
// triangle node pointer: (index >> 2) << 3 | (index & 3). Triangles no node
// points at are NaN (inactive). A top level's instance node at node index n
// is instance n of u1, so a host hit's instance index names the title's
// instance node pointer: index << 3 | 6. Slots of no instance node are
// zeros (inactive).

#define RS "RootConstants(num32BitConstants=8, b0), UAV(u0), UAV(u1), UAV(u2), UAV(u3), SRV(t0)"

cbuffer Constants : register(b0) {
    uint words;     // gather: dwords to copy
    uint top;       // extract: 1 a top level, 0 a bottom level
    uint capacity;  // extract: nodes of 64 bytes u0 holds (and u1 is made for)
    uint entries;   // extract, top level: entries of t0
    uint slot;      // extract: the statistics' slot
    uint queued;    // extract: entries u2 holds
    uint copied;    // extract: nodes of 64 bytes copied into u0
    uint pad1;
};

RWByteAddressBuffer Source : register(u0);
RWByteAddressBuffer Out : register(u1);
RWByteAddressBuffer Queue : register(u2);
RWByteAddressBuffer Stats : register(u3);
ByteAddressBuffer Map : register(t0);

#define THREADS 1024
#define LEVELS 64
#define NONE 0xffffffffu

// Statistics (dwords of a slot).
#define S_TRIANGLES 0
#define S_BOXES 1
#define S_INSTANCES 2
#define S_UNKNOWN 3
#define S_ERRORS 4
#define S_OVERFLOW 5
#define S_LEFT 6
#define S_LIMIT 7
#define S_BOUNDS 8      // 6: minimum x y z, maximum x y z, ordered
#define S_HEADER 16     // 16: the header's dwords

[RootSignature(RS)]
[numthreads(256, 1, 1)]
void gather(uint3 id : SV_DispatchThreadID) {
    if (id.x < words) {
        Out.Store(id.x * 4, Source.Load(id.x * 4));
    }
}

groupshared uint head;
groupshared uint tail;
groupshared uint limit;

void stat(uint at, uint n) {
    uint ignored;
    Stats.InterlockedAdd((slot * 32 + at) * 4, n, ignored);
}

// A float's bits, ordered as unsigned integers order.
uint ordered(float x) {
    uint b = asuint(x);
    return (b & 0x80000000u) != 0 ? ~b : b | 0x80000000u;
}

void bound(float3 v) {
    uint ignored;
    uint at = (slot * 32 + S_BOUNDS) * 4;
    Stats.InterlockedMin(at + 0, ordered(v.x), ignored);
    Stats.InterlockedMin(at + 4, ordered(v.y), ignored);
    Stats.InterlockedMin(at + 8, ordered(v.z), ignored);
    Stats.InterlockedMax(at + 12, ordered(v.x), ignored);
    Stats.InterlockedMax(at + 16, ordered(v.y), ignored);
    Stats.InterlockedMax(at + 20, ordered(v.z), ignored);
}

float3 vertex(uint node, uint n) {
    return asfloat(Source.Load3(node * 64 + n * 12));
}

void emit_triangle(uint p) {
    uint node = p >> 3;
    uint k = p & 3;
    if (node == 0 || node >= limit) {
        stat(S_ERRORS, 1);
    } else {
        // Triangle k's vertices (crate bvh::TRIANGLE_VERTICES), by the
        // triangle's own vertex numbers; a number 3 reads vertex 0, as the
        // console's arithmetic lands there.
        uint map = k == 0 ? 0x210u : k == 1 ? 0x231u : k == 2 ? 0x432u : 0x042u;
        uint id = Source.Load(node * 64 + 60) >> (8 * k);
        uint vi = id & 3;
        uint vj = (id >> 2) & 3;
        uint first = (3 - vi - vj) & 3;
        float3 a = vertex(node, (map >> (4 * first)) & 0xf);
        float3 b = vertex(node, (map >> (4 * vi)) & 0xf);
        float3 c = vertex(node, (map >> (4 * vj)) & 0xf);
        uint at = (node * 4 + k) * 36;
        Out.Store3(at, asuint(a));
        Out.Store3(at + 12, asuint(b));
        Out.Store3(at + 24, asuint(c));
        stat(S_TRIANGLES, 1);
        bound(a);
        bound(b);
        bound(c);
    }
}

// The host address of the bottom level at title address `base` << 8 (0: not
// known).
uint2 host_of(uint base) {
    uint low = 0;
    uint high = entries;
    [loop] while (low < high) {
        uint mid = (low + high) / 2;
        uint key = Map.Load(mid * 12);
        if (key < base) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    uint2 va = uint2(0, 0);
    if (low < entries && Map.Load(low * 12) == base) {
        va = Map.Load2(low * 12 + 4);
    }
    return va;
}

void emit_instance(uint p) {
    uint node = p >> 3;
    if (node == 0 || node + 1 >= limit) {
        stat(S_ERRORS, 1);
    } else {
        // The first 64 bytes are a D3D12 instance description but for the
        // structure: the bottom level's root pointer, then its address in
        // 256-byte units. (Bytes 64-111: the inverse transform.)
        uint4 d0 = Source.Load4(node * 64);
        uint4 d1 = Source.Load4(node * 64 + 16);
        uint4 d2 = Source.Load4(node * 64 + 32);
        uint4 d3 = Source.Load4(node * 64 + 48);
        uint2 va = host_of(d3.w);
        if (va.x == 0 && va.y == 0) {
            // Not known: inactive, and no mask to hit with.
            d3.x &= 0x00ffffffu;
            stat(S_UNKNOWN, 1);
        }
        uint at = node * 64;
        Out.Store4(at, d0);
        Out.Store4(at + 16, d1);
        Out.Store4(at + 32, d2);
        Out.Store4(at + 48, uint4(d3.x, d3.y, va.x, va.y));
        stat(S_INSTANCES, 1);
        float3 t = asfloat(uint3(d0.w, d1.w, d2.w));
        bound(t);
    }
}

void visit(uint p) {
    uint kind = p & 7;
    if (p == NONE) {
    } else if (kind == 4 || kind == 5) {
        // Dword 0 of the queue counts what was put in it.
        // Past the end the last entry is written over, and the end counts
        // it.
        uint q;
        Queue.InterlockedAdd(0, 1, q);
        Queue.Store(4 + min(q, queued - 1) * 4, p);
    } else if (kind < 4 && top == 0) {
        emit_triangle(p);
    } else if (kind == 6 && top != 0) {
        emit_instance(p);
    } else {
        stat(S_ERRORS, 1);
    }
}

void box(uint p) {
    uint node = p >> 3;
    if (node == 0 || node + (p & 7) - 4 >= limit) {
        stat(S_ERRORS, 1);
    } else {
        stat(S_BOXES, 1);
        uint4 children = Source.Load4(node * 64);
        visit(children.x);
        visit(children.y);
        visit(children.z);
        visit(children.w);
    }
}

[RootSignature(RS)]
[numthreads(THREADS, 1, 1)]
void extract(uint3 id : SV_GroupThreadID) {
    uint t = id.x;
    // dw1: the structure's size in nodes past the header.
    uint nodes = Source.Load(4) + 1;
    uint lim = min(nodes, min(copied, capacity));
    if (t < 16) {
        Stats.Store((slot * 32 + S_HEADER + t) * 4, Source.Load(t * 4));
    }
    if (t == 0) {
        Stats.Store((slot * 32 + S_LIMIT) * 4, lim);
        head = 0;
        tail = 0;
        limit = lim;
        Queue.Store(0, 0);
    }
    // Nothing is left of an earlier build.
    if (top == 0) {
        uint n = capacity * 4 * 9;
        [loop] for (uint i = t; i < n; i += THREADS) {
            Out.Store(i * 4, 0x7fc00000u);
        }
    } else {
        uint n = capacity * 16;
        [loop] for (uint i = t; i < n; i += THREADS) {
            Out.Store(i * 4, 0);
        }
    }
    AllMemoryBarrierWithGroupSync();
    if (t == 0) {
        // dw12: the root.
        visit(Source.Load(48));
    }
    AllMemoryBarrierWithGroupSync();
    if (t == 0) {
        tail = min(Queue.Load(0), queued);
    }
    AllMemoryBarrierWithGroupSync();
    // ([fastopt]: else the compiler would unroll it for the atomic in the
    // loop in it, and cannot.)
    [loop] [fastopt] for (uint level = 0; level < LEVELS; level++) {
        uint h = head;
        uint e = tail;
        [loop] for (uint i = h + t; i < e; i += THREADS) {
            box(Queue.Load(4 + i * 4));
        }
        AllMemoryBarrierWithGroupSync();
        if (t == 0) {
            head = e;
            tail = min(Queue.Load(0), queued);
        }
        AllMemoryBarrierWithGroupSync();
    }
    if (t == 0) {
        if (tail > head) {
            stat(S_LEFT, tail - head);
        }
        uint all = Queue.Load(0);
        if (all > queued) {
            stat(S_OVERFLOW, all - queued);
        }
    }
}
