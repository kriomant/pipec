# Pipec

Pipec is a terminal application designed to edit command pipelines.

## Purpose

I often need to execute complex pipelines: query some API, extract information using *jq*, execute some commands based on it:

```sh
curl -s https://some.api/fetch?limit=100 | jq -r '.result.items[] | .host.fqdn' | xargs -n1 -I% -- ssh '%' grep ERROR /var/log/service.log
```

Editing such pipelines directly in shell has some inconveniences:
* It is hard to navigate between commands when command line becomes too long;
* Each stage of pipeline is executed every time, even when it isn't changed. This is especially sad when some stage takes a long time to run;
* It is hard to preview output of any internal stage.

## Screenshot

![](assets/screenshot.png)

## Demo

![Demo](https://vhs.charm.sh/vhs-5roNbHxTOm2RD2ZFeIdxrr.gif)

## Features

* Minimalistic user interface
* Output of stages is cached and reused on next runs.
  Don't wait for long API response or database query every time.
* Preview output of any stage in pipeline.
* Possible shell integration: press key to edit existing pipeline, get updated one on exit.
* Command syntax highlighting

## Guide

### Installation

#### From source

```sh
cargo install --locked pipec
```

#### MacOS

```sh
brew tap kriomant/tap
brew install pipec
```

#### Linux

Install binary from GitHub Releases

### Basics

Just running `pipec` will open empty pipeline.

Basic help with hotkeys and legend is shown on start, so there shouldn't be any problem to start using *pipec*.

Type command and press **Enter** to execute. Use **Ctrl-Q** to exit. You will be asked whether you want to print resulting pipeline or it's output.

You can also copy various items to clipboard with **Ctrl-Y**.

**Warning**: All output of each stage is saved into memory. Don't use it with commands which produces a lot of output.

### Focused and shown stages

Stage you are editing is *focused* one. You may change focused stage with *↑*/*↓* keys. By default commands are executed up to focused one.

However, it is often convenient to preview output of some earlier or later stage when composing current one. So there is concept of *shown* stage. You select stage to show using **Ctrl-Space** key.

### Starting with existing pipeline

*Pipec* accepts any number of positional arguments. Each argument is turned into separate stage command:

```sh
pipec 'ls -l' 'grep .toml'
```

If you don't want to split pipeline into commands manually, use may use `--parse-commands` option to split passed pipelines into commands:

```sh
pipec --parse-commands 'ls -l | grep .toml'
```

### Shell integration

Most convenient way to use *pipec* is to integrate it with shell.

#### zsh

You may put following to you *~/.zshrc* to edit currently typed command with *pipec* by pressing **Ctrl-X P**:

```sh
edit-pipeline-widget() {
  local OUTPUT=$(cd ~/path/to/pipec; cargo run -- --parse-commands --print-on-exit=pipeline -- "$BUFFER")
  if [ $? -eq 0 ]; then
    BUFFER="$OUTPUT"
  fi
}
zle -N edit-pipeline-widget
bindkey '^xp' edit-pipeline-widget
```

If you know how to implement that for other shells beside *zsh*, tell me.
