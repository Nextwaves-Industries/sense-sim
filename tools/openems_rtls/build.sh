#!/bin/sh
# Builds assets/openems/sensesim-rtls.jar, the OpenEMS bundle that holds the
# RTLS channels (Rtls.SenseSim) and the GPU meter (Gpu.SenseSim). Needs a
# JDK 21 (JAVA_HOME) and an openems-edge.jar to compile
# against (its embedded jar/*.jar are the API).
#
#   JAVA_HOME=/path/to/jdk-21 ./tools/openems_rtls/build.sh path/to/openems-edge.jar
set -e
here=$(cd "$(dirname "$0")" && pwd)
edge=${1:?usage: build.sh path/to/openems-edge.jar}
out="$here/../../assets/openems/sensesim-rtls.jar"
work=$(mktemp -d)
# Native Windows javac needs Windows paths.
win() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else echo "$1"; fi; }
(cd "$work" && unzip -q -o "$edge" 'jar/*')
sep=":"; case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) sep=";";; esac
cp="$(win "$work/jar/io.openems.edge.common.jar")$sep$(win "$work/jar/io.openems.edge.meter.api.jar")$sep$(win "$work/jar/io.openems.common.jar")$sep$(win "$(ls "$work"/jar/org.osgi.service.component-*.jar)")$sep$(win "$(ls "$work"/jar/org.apache.felix.framework-*.jar)")$sep$(win "$(ls "$work"/jar/org.apache.felix.eventadmin-*.jar)")"
mkdir -p "$work/classes/OSGI-INF/metatype" "$(dirname "$out")"
"$JAVA_HOME/bin/javac" --release 21 -nowarn -cp "$cp" -d "$(win "$work/classes")" "$here"/src/com/nextwaves/sensesim/*/*.java
cp "$here/OSGI-INF/"*.xml "$work/classes/OSGI-INF/"
cp "$here/OSGI-INF/metatype/"*.xml "$work/classes/OSGI-INF/metatype/"
"$JAVA_HOME/bin/jar" --create --file "$out" --manifest "$here/MANIFEST.MF" -C "$work/classes" .
rm -rf "$work"
echo "built $out"
