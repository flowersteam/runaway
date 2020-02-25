<div align="center">
<img align="center" src="https://runaway.gitlabpages.inria.fr/assets/img/hollow_logo.svg">
<h1 align="center">Runaway</h1>
 <strong>
   A simple tool to help you in deploying experimental campaigns to high-performance clusters.
 </strong>
</div>

<br />

<div align="center">
  <h3>
    <a href="https://runaway.gitlabpages.inria.fr/home">
      Home
    </a>
    <span> | </span>
    <a href="https://runaway.gitlabpages.inria.fr/docs/install/">
      Docs
    </a>
    <span> | </span>
    <a href="https://runaway.gitlabpages.inria.fr/docs/tutorial">
      Tutorial
    </a>
    <span> | </span>
    <a href="https://gitlab.inria.fr/runaway/runaway/-/releases">
      Releases
    </a>
  </h3>
</div>



Runaway is a command line-tool, developed in the flowers team at Inria, that 
allows you to launch a significant number of simultaneous executions of your 
code on a remote resource, with one single call. Whatever the scale and the 
resource, results are one command away from your laptop.

## Installation

Runaway is available for linux and osx platforms. To install it on your computer
follow the procedure in the [install section of the documentation](https://runaway.gitlabpages.inria.fr/docs/install/).

## Examples: 

To perform a remote execution in one line:
```shell
$> runaway exec localhost train.py -- --dry-run --epoch=1
  runaway: Loading host
  runaway: Reading arguments
  runaway: Reading ignore files
  runaway: Compress files
  runaway: Acquiring node on the host
  runaway: Transferring data
  runaway: Extracting data in remote folder
  runaway: Removing archive
  runaway: Executing script
  
  Training model
  
  ...
  
  runaway: Compressing data to be fetched
  runaway: Transferring data
  runaway: Extracting archive
  runaway: Cleaning data on remote
$> 
```

To run your experimental campaign: 
```shell
$> runaway batch localhost train.py -A parameters.txt
  runaway: Loading host
  runaway: Reading arguments
  runaway: Reading ignore files
  runaway: Compress files
  runaway: Transferring data
  runaway: Starting execution with arguments "--lr=0.1"
  runaway: Starting execution with arguments "--lr=0.01"
  
  10eb724a-e9b4-441b-bbe2-e197a11d628d: Training model 
  da177f52-bb68-4799-8d82-6b511d557181: Training model
  
  ...
   
  runaway: Cleaning data on remote
$> 
```

To automate `runaway` with a script of your own:
```shell
$> runaway sched my-cluster train.py "./sched.py --reschedule"
  runaway: Loading host
  runaway: Reading arguments
  runaway: Reading ignore files
  runaway: Compress files
  runaway: Transferring data
  runaway: Querying the scheduler
  runaway: Starting execution with arguments "--lr=0.1 --no-augment "
 
  31908337-10bc-485a-806c-e3ef38cf6d1e: Training model
  ...
  
  runaway: Querying the scheduler
  runaway: Starting execution with arguments "--lr=0.01 "
  runaway: Starting execution with arguments "--lr=0.1 "
  runaway: Querying the scheduler
  ...
```

## Credits

This work was founded by INRIA under a ADT grant.

