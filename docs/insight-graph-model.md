# Insight Graph Model

This document defines the graph schemas used by AL Insight.

## Graph Types
1. CallGraph: procedure to procedure edges (call hierarchy).
2. EventGraph: publisher to subscriber edges.
3. ObjectGraph: object to object relationships (extends, implements, depends).
4. TableRelationGraph: table to table edges (relations via fields, keys, and usage).

## Node Schema
1. `id`: stable identifier.
2. `kind`: object, procedure, event, table, field.
3. `name`: display name.
4. `package`: originating package.
5. `file`: source file path if workspace.
6. `span`: optional text range.

## Edge Schema
1. `from`, `to`: node ids.
2. `kind`: call, publish, subscribe, extend, relation.
3. `weight`: numeric score.
4. `metadata`: map for extra context.

## Indexing Inputs
1. Workspace AST from `al-syntax`.
2. Package symbols from `al-symbols`.
3. Semantic metadata from `al-semantic`.

## Index Update Strategy
1. Incremental updates on file change.
2. Batch rebuild on package downloads or toolchain changes.
3. Cache graphs per workspace.
