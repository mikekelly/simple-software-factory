#!/bin/bash
# Install Tailscale only when a person asks for it, then enrol this guest.
# The control plane makes requested hostnames unique within the tailnet.
set -euo pipefail

hostname=${1:-ssf-vm}

if ! command -v tailscale >/dev/null 2>&1; then
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *\ ubuntu\ *)
            repo_os=ubuntu
            codename=${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}
            ;;
        *\ debian\ *)
            repo_os=debian
            codename=${VERSION_CODENAME:-}
            ;;
        *\ arch\ *)
            sudo pacman -Syu --noconfirm --needed tailscale
            repo_os=
            codename=
            ;;
        *)
            echo "tailscale: unsupported guest distribution ${ID:-unknown}; install Tailscale in the guest, then rerun this command" >&2
            exit 1
            ;;
    esac

    if [ -n "$repo_os" ]; then
        if [ -z "$codename" ]; then
            echo "tailscale: could not determine this $repo_os release codename" >&2
            exit 1
        fi
        key_url="https://pkgs.tailscale.com/stable/$repo_os/$codename.noarmor.gpg"
        list_url="https://pkgs.tailscale.com/stable/$repo_os/$codename.tailscale-keyring.list"
        tmp=$(mktemp)
        trap 'rm -f "$tmp"' EXIT
        curl -fsSL -o "$tmp" "$key_url"
        sudo install -Dm644 "$tmp" /usr/share/keyrings/tailscale-archive-keyring.gpg
        curl -fsSL -o "$tmp" "$list_url"
        sudo install -Dm644 "$tmp" /etc/apt/sources.list.d/tailscale.list
        sudo apt-get update
        sudo apt-get install -y tailscale
        rm -f "$tmp"
        trap - EXIT
    fi
fi

sudo systemctl enable --now tailscaled.service
status=$(sudo tailscale status --json 2>/dev/null || true)
if printf '%s' "$status" | jq -e '.BackendState == "Running"' >/dev/null 2>&1; then
    sudo tailscale set --hostname="$hostname"
    echo "Tailscale was already enrolled; its requested hostname is now $hostname."
else
    echo "Open the Tailscale login URL below to enrol this VM."
    sudo tailscale up --hostname="$hostname"
fi

status=$(sudo tailscale status --json)
dns_name=$(printf '%s' "$status" | jq -r '.Self.DNSName // .Self.HostName // empty')
ip=$(sudo tailscale ip -4 2>/dev/null || true)
dns_name=${dns_name%.}
printf 'Tailscale: enrolled as %s%s\n' "${dns_name:-$hostname}" "${ip:+ ($ip)}"
