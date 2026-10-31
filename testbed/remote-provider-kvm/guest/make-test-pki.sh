#!/usr/bin/env bash
# Throwaway test PKI for the remote provider KVM acceptance (#263).
#
#   make-test-pki.sh <empty-output-dir> <mock-name> <redirect-name> <wrong-name>
#
# TEST ONLY, NOT FOR PRODUCTION. Generated fresh for every run, inside the
# disposable guest (or a scratch directory in the offline tests); no key is
# ever committed or copied out. Produces:
#   test-ca.crt          trusted by the guest for this run only
#   mock.crt/.key        <mock-name> and <redirect-name>, signed by test-ca
#   wrong-name.crt/.key  <wrong-name> only, signed by test-ca
#   untrusted-mock.*     <mock-name>, signed by a CA nobody trusts
set -euo pipefail
umask 077

[[ $# -eq 4 ]] || { echo "usage: $0 <empty-output-dir> <mock-name> <redirect-name> <wrong-name>" >&2; exit 2; }
out=$1 mock=$2 redirect=$3 wrong=$4
[[ -d $out && -z $(ls -A "$out") ]] || { echo "make-test-pki: $out must be an existing empty directory" >&2; exit 1; }
for name in "$mock" "$redirect" "$wrong"; do
    [[ $name =~ ^[a-z0-9.-]+\.test$ ]] || { echo "make-test-pki: $name is not a .test name" >&2; exit 1; }
done
command -v openssl >/dev/null || { echo "make-test-pki: openssl is required" >&2; exit 1; }

label="FLOWERLY TEST ONLY - NOT FOR PRODUCTION"

ca() {
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
        -subj "/O=$label/CN=$1" -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" \
        -keyout "$out/$2.key" -out "$out/$2.crt" 2>/dev/null
}

leaf() {
    local name=$1 ca=$2 sans=$3
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -subj "/O=$label/CN=$name" -keyout "$out/$name.key" -out "$out/$name.csr" 2>/dev/null
    printf 'subjectAltName=%s\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n' "$sans" >"$out/$name.ext"
    openssl x509 -req -in "$out/$name.csr" -CA "$out/$ca.crt" -CAkey "$out/$ca.key" \
        -CAcreateserial -days 2 -extfile "$out/$name.ext" -out "$out/$name.crt" 2>/dev/null
    rm -f "$out/$name.csr" "$out/$name.ext"
}

ca "flowerly kvm acceptance test CA" test-ca
ca "flowerly kvm acceptance UNTRUSTED CA" untrusted-ca
leaf mock test-ca "DNS:$mock,DNS:$redirect"
leaf wrong-name test-ca "DNS:$wrong"
leaf untrusted-mock untrusted-ca "DNS:$mock"
# CA keys are not needed after signing.
rm -f "$out/test-ca.key" "$out/untrusted-ca.key" "$out"/*.srl
chmod 0644 "$out"/*.crt
echo "test PKI written to $out (TEST ONLY)"
