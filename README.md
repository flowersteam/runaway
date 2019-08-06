# Orchestra tools

Ochestra is a project that aims at providing a set of tools to help performing experimental campaigns in computer science. In particular, we would like to address the following points:

- Running a large set of experiments on a large cluster should be as simple as running one such experiment on your laptop.
- Hyper-Parameter search should be left to algorithms rather than students.
- Experimental code should be versioned. Results should be linked to code version.
- Collaboration on experimental campaigns should be hassle free.
- Newcomers should be able to crank-up their campaign in a day. 

To address those points, we work on a few tools developed on the same backbone library:

- runaway: a command-line tool to execute code on distant clusters as simply as possible.
- expegit: a command-line tool to organize an experimental campaign as a repository, allowing code versioning, results versioning, and collaboration.
- parasearch: a command-line tool to automate hyper-parameter search using a few common algorithms. 
- orchestra: a tool to manage the whole lifecycle of an experimental campaign addressed in the preceding other tools, through a simple web ui.

## Status

In 2018 a first prototype of those tools was written, but a foundational design decision prevented the tools to scale to massive campaigns. In the 0.1.0 release, a new backbone library was introduced, allowing the expected performance. Only a new version of runaway was shipped with this release, and we are currently working toward updating the other tools. The project is currently under active development, and breaking changes should be expected until mid 2020.

## Current release

The current release of the orchestra tools is 0.1.

Most of the work for this version was focused on a new implementation of the backbone library `liborchestra`  featuring:

- A shift to `futures`-based concurrency in the whole codebase. From the ssh connection to the resource allocation, slot acquisition, and repository interactions; every blocking operations involved in the execution was made non-blocking. This allows to concurrently execute as much executions as allowed by the different resources (ssh connections, schedulers, nodes), and not by the task scheduler.
- A concurrent model of cluster schedulers. The scheduling of executions that was once deferred to the remote schedulers such as slurms, is now concurrently managed from the library. This allows to substitute for the platform queue, locally and concurrently.
- A fine-grained slot acquisition model. The placement of executions processes on nodes that was once deferred to the scheduler is now managed by the library. This means that we can acquire 10 nodes and place any number (e.g. threds number) of execution processes on every single nodes independently and concurrently. This allows for a much more intensive use of the acquires resources, while retaining the gain of managing executions one by one.
- Much more :)

## Install

To install the tools, consult the [wiki](https://gitlab.inria.fr/apere/orchestra/wikis/home)







