# Orchestra

Ochestra is a set of tools meant to help in performing experimental campaigns in computer science. It provides you with 
simple tools to:
+ Organize your clumsy experimental workflow, leveraging our beloved `git` and `lfs` through a simple interface.
+ Collaborate with other peoples on a single experimental campaign. 
+ Execute pieces of code on remote hosts such as clusters or clouds, in one line.
+ Automate the execution of batches of experiments and the presentation of the results through a clean web ui.

A lot of advanced tools exists on the net to handle similar situations. Most of them target very complicated workflows, 
e.g. DAGs of tasks. Those tools are very powerful but lack the simplicity needed by newcomers. Here, we propose a 
limited but very simple tool to handle one of the most common situation of experimental campaigns: the repeated 
execution of an experiment on variations of parameters.

What do you need to use orchestra tools?
+ Git, [Lfs](https://git-lfs.github.com/), and Openssh-client installed on your computer
+ An instance of a [git-repository manager that supports LFS](https://docs.gitlab.com/ee/workflow/lfs/manage_large_binaries_with_git_lfs.html)

Need an example? 

## Quick Start

The preferred way is through the web interface, but if you prefer the command line, after setting up 
the tools, you should be able to go through the following steps:

Assuming you have a repository containing your experimental code, create a run handle:
```bash
$ cd my_experiment
$ ln -s my_script.py run && chmod +x run
$ git add run && git commit -m "Adds orchestra run handle" && git push
```

We let orchestra create a campaign repository at a new address for us:
```bash
$ orchestra init ssh://git@gitlab.com:user/my_experiment.git ssh://git@gitlab.com:user/my_new_campaign.git .
```

Now, we run a bunch of experiments:
```bash
$ orchestra jobs run my-cluster "" " --param¤first;second¤--flag;"

orchestra: using git 2.18.0 and lfs 2.4.2
orchestra: generating jobs
orchestra: no commit hash encountered. Continuing with HEAD ...
orchestra: parameters  --param¤first;second¤--flag; encountered
orchestra: start run with parameters ' --param first --flag'
orchestra: start run with parameters ' --param first '
orchestra: start run with parameters ' --param second --flag'
orchestra: waiting for tasks completion
orchestra: tasks completed
```

After the end of the runs, you should be able to navigate your results at the address 
`https://gitlab.com/user/my_new_campaign/tree/master/excs` or, in your local repository at `./my_new_campaign/excs`.

## Going further

As we can see on the previous examples, Orchestra allows you to handle two issues when facing an experimental campaign:
+ The storage of the results, which is made through a _campaign_ git repository.
+ The (asynchronous) execution of experiments on remote hosts, which is parameterized by profiles.

Those two issues are adressed by two lower-level tools:
+ __Expegit__: a command line tool to manage campaign repositories
+ __Runaway__: a command line tool to excute code on remote hosts

In essence, the __Orchestra__ tool is just a frontend that automates those two lower level tools. Let's dive deeper into 
those.

### Managing experimental campaign results with Expegit

Expegit is a command line tool that allows to use a git repository to store the results of your experimental campaign. 
As you may know, Git is not meant to handle large binary files, which you'll have plenty of in your results. To use 
git to handle those, we make use of [Large File Storage](https://git-lfs.github.com/), which allows to avoid 
storing large binary files containing results, as diffs. In essence, _Expegit_ just wraps some git commands in an 
application to provide a comprehensive workflow for experimental campaigns. You can think of it as something similar to 
gitflow, for experimental campaign. 

We consider that you are writing the code of you experiment inside an other repository, which we will call the 
_experiment repository_. To initialize the campaign repository in a local and empty repository, simply run:
```bash
expegit init -p ssh://git@gitlab:user/my_experiment.git local-empty-campaign-repository  
```
That's it! By doing that, you have created a few folders in your local campaign repository:
``` 
local-campaign-repository/     # Your local campaign repository
│
├── xprp/                      # Your experiment repository as a git submodule, with some experimental code of yours
│   ├── experiment.sh
│   └── ...                    
├── excs/                      # An empty foler that will contain experiment executions
│   └── ...        
└── ...
```
The `-p` flag should have pushed your changes to the origin, so if you navigate to `https://gitlab.com/user/my_experiment`, 
you should see the same architecture.

Now, let's create a new experiment:

```bash
expegit exec new -p 7371ecc9232fd03ea0ecece7ab9db12171ef9d6f -- param_1 --param2="test" --param3 
``` 
Wuw, what does that means? We create a new _execution_ of the experiment, that will correspond to the experiment 
repository at commit `7371ecc9232fd03ea0ecece7ab9db12171ef9d6f`. Plus, we give a list of parameters 
`param_1 --param2="test" --param3`, that could be retrieved to be fed at your script at execution time. By performing 
this, you have created a new folder in the `excs` directory:
```
campaign-repository/            
├── excs/                                    
│   └── 89cfd4a9-f42b-482a-9340-c5d762ea6f73  # The execution folder, which is given an identfier
│       ├── data/                             # The data folder where experimental data will be stored
│       │   └── lfs/                          # The special lfs that will use lfs storing for every data it contains
│       │       └── .gitattributes            
│       ├── experiment.sh                    #  Your experimental code at 7371ecc9232fd03ea0ecece7ab9db12171ef9d6f
│       └── ...
└── ...
```
As you see, your experimental script `experiment.sh` must be made so as to store experimental data directly in `data` 
for those needing lfs and in `data/lfs` for those that don't.

You can now run your experiment on your own, by retrieving the parameters out of expegit:
```bash
cd campaign-repository/excs/89cfd4a9-f42b-482a-9340-c5d762ea6f73
./experiment.sh $(expegit exec params 89cfd4a9-f42b-482a-9340-c5d762ea6f73)
``` 

We only have to end up the experiment by running:
```bash
cd .. 
expegit finish -p 89cfd4a9-f42b-482a-9340-c5d762ea6f73
```

This command allows to delete the unnecessary files, such as all the (non-results) files from the experiment, and pushes
the results to the remote. If you now visit `https://gitlab.com/user/my_campaign/tree/master/excs/89cfd4a9-f42b-482a-9340-c5d762ea6f73`,
you should see the results generated by your script. Your execution is now over, and you can repeat the process :)

### Executing code on remote hosts with Runaway

Runaway is a second command line tool which allows to run a script on a remote host that you can reach via ssh. It 
basically automates the upload of the files, the execution of the code, and the fetching of the results. 

Let's take an example. Imagine we have a directory containing a python script and some modules it uses:
```
my-experiment/            
├── src/                                    
│   └── some modules ...
├── script.py
└── ...
```

To run `script.py` on our cluster, we run:
```
$ cd my-experiment
$ runaway cluster script.py -- --param=1 --flag
```

By doing this, we perform three things:
+ If the code is not already there, the content of `my-experiment` folder is sent to our cluster 
+ `script.py --param=1 --flag` is executed on remote
+ The files created in the `my-experiment` folder on the cluster, are fetched to our local directory

A few remarks:
+ Unless we use a `-v` argument, the stderr, stdout and exit code of the runaway command copy the ones of the remote 
execution
+ Since only the content of the folder can be sent, the resources used by our code should either be in the folder, or
exist and be accessible on the cluster we execute on.

How is the cluster parameterized, by the way? We can write our own execution profile as `.yml` files which are stored in 
`~/.runaway`. For example, a profile to execute on our own computer would be something like that:
```yaml
# Name of the ssh config to use. Must be defined in your ~/.ssh/config.
ssh_config: localhost

# Path to the host directory in which to store the code.
host_directory: /home/user/Executions

# Bash commands to execute before the script.
before_execution:
  - echo Preparing Execution
  - echo Executed on $HOSTNAME
  - export PATH="/home/user/.local/share/anaconda3/bin:$PATH"
  - chmod +x $SCRIPT_NAME

# Bash command to execute the script. The following environment variables are replaced at run time:
#     + `$SCRIPT_NAME`: the file name of the script.
#     + `$SCRIPT_ARGS`: the arguments of the script.
execution:
  - echo Starting Execution
  - source /home/user/.bashrc; $SCRIPT_NAME $SCRIPT_ARGS

# Bash commands to execute after the script.
after_execution:
  - echo 'Cleaning Execution'

```

As we can see, the remote must be accessible via ssh and configured in your `.ssh/config`. An other example of such a 
profile to run on a slurm cluster would be:
```yaml
# Name of the ssh config to use. Must be defined in your ~/.ssh/config.
ssh_config: cluster-ext

# Path to the host directory in which to put the code.
host_directory: /home/user/Executions

# Bash commands to execute before the script.
before_execution:
  - echo Preparing Execution
  - echo Executed on $HOSTNAME
  - module load slurm
  - module load language/intelpython/3.6

# Bash command to execute the script. The following environment variables are replaced at run time:
#     + `$SCRIPT_NAME`: the file name of the script.
#     + `$SCRIPT_ARGS`: the arguments of the script.
execution:
  - echo Starting Execution
  - srun -p long-queue --verbose $SCRIPT_NAME $SCRIPT_ARGS

# Bash commands to execute after the script.
after_execution:
  - echo 'Cleaning Execution'
```

As you can imagine, sending and fetching the whole folder back and forth can be resource consuming. Runaway proposes two
ways to temper this:
+ Code Reuse: The code is sent to remote as a tarball archive, which is compared with archives already existing on the 
remote. If the remote already contains this archive, then it will skip the sending and will directly used the one there. 
Basically, runaway cleans the whole remote execution at the end of the run `--leave=nothing`, but you can leave either 
the code or the code+results with `--leave=code` and `--leave=everything` respectively.
+ Files Ignoring: When packing the files the be sent to the remote, files and folders can be ignored with the 
`.sendignore` file. This one is nothing but a simple text file containing patterns and globs, in the same way as a 
`.gitignore` file. For example if you are at the root of a git repository, adding your `.git` folder in the 
`.sendignore` may be interesting. The same ignoring can be parameterized for the fetching of the results in the 
`.fetchignore` file. If you are in an expegit execution, you can parameterize this one to only fetch the `data` folder 
for example. Beware to not ignore your `.fetchignore` in your `.sendignore` !

### Automating Expegit and Runaway with Orchestra

The two previous tools allows to handle the results and the run of a single experiment execution. In the case of an 
experimental campaign, we are, in genereal, interested in automating those stuffs to run large batches of experiments. 
Orchestra allows to automate the creation and execution of expegit executions through a simple interface. You can choose
to use orchestra only via command line, but the prefered way is to use the web ui:
```bash
$ orchestra -v gui
```

This will launch an application that allows you to generate batch of executions, and to monitor and access the results.
Indeed, every images (plots, gifs, ...) is rendered in the execution report at `https://localhost:8088/executions/{id}`.
This allows you to quickly check the results of a single experiment. As you may expect, the results are automatically 
synchronized with the remote repository.

Moreover, thanks to the use of Git to store the results, a same remote repository can be fed results by multiple 
Orchestra instances. This allows you to execute experiments from different machines which may not have access to the 
same computational ressources. On such case appears if multiple people collaborate on an experimental campaign, with 
access to different ressources. This quickly becomes the case with clusters, since your number of simultaneous 
executions may be limited by your user account. With Orchestra, you can multiply your limit by running executions from 
the computers of the different people working on your campaign.


## Install

To install orchestra tools, simply go to the [release page]() of the wiki. There you'll be able to download the binaries 
of the three tools, which you only need to make available in your PATH. As of today, only linux and osx binaries are 
available. 

### Troubleshoot:

Depending on the version of LFS used by your repository manager, it may not accept LFS files with content-types. You can 
set this off with `git config --global lfs.contenttype false`.

## Todo

Orchestra is far from being complete. Here are some ideas which may be implemented in the future:
+ Automating the execution of campaign wide analysis at new result
+ Automating the generation of runs, based on the existing results, using exploration, bayesian optimization or 
diversity search.
+ ...

## About

Orchestra-tools are written in Rust. Though this choice was led by pure curiosity at first, this language proved to be 
quite handy to produce reliable tools. Despite its strong emphasis on speed and memory-safety, Rust also benefits from a
set of nice abstractions to enforce good practices in software development (Ownership, Options, Results, etc..). 
Moreover, when the right library is available, writing code in rust is not longer than with a scripting language such as 
python. Because of its young age, rust does not provide such high level librairies for `git` and `ssh`, which are 
consequently managed through processes.








