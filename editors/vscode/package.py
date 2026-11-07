#!/usr/bin/env python3
"""VS Code の拡張機能（.vsix）を作る。npm も vsce も要らない。

    python3 editors/vscode/package.py

.vsix は、決まった形の zip ファイル（中に extension.vsixmanifest と extension/ のファイル）。
"""

import json
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))

# 拡張機能に入れるファイル（zip の中の名前, このフォルダからのパス）
FILES = [
    ("extension/package.json", "package.json"),
    ("extension/language-configuration.json", "language-configuration.json"),
    ("extension/syntaxes/orz.tmLanguage.json", "syntaxes/orz.tmLanguage.json"),
    ("extension/README.md", "README.md"),
    ("extension/LICENSE.txt", "LICENSE"),
]

CONTENT_TYPES = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
  <Default Extension=".md" ContentType="text/markdown"/>
  <Default Extension=".txt" ContentType="text/plain"/>
</Types>
"""

MANIFEST = """<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="{name}" Version="{version}" Publisher="{publisher}" />
    <DisplayName>{display}</DisplayName>
    <Description xml:space="preserve">{description}</Description>
    <Tags>orz</Tags>
    <Categories>Programming Languages</Categories>
    <GalleryFlags>Public</GalleryFlags>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="{engine}" />
      <Property Id="Microsoft.VisualStudio.Code.ExtensionDependencies" Value="" />
      <Property Id="Microsoft.VisualStudio.Code.ExtensionPack" Value="" />
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="ui,workspace" />
      <Property Id="Microsoft.VisualStudio.Code.LocalizedLanguages" Value="" />
      <Property Id="Microsoft.VisualStudio.Services.GitHubFlavoredMarkdown" Value="true" />
    </Properties>
    <License>extension/LICENSE.txt</License>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/README.md" Addressable="true" />
    <Asset Type="Microsoft.VisualStudio.Services.Content.License" Path="extension/LICENSE.txt" Addressable="true" />
  </Assets>
</PackageManifest>
"""


def main():
    with open(os.path.join(HERE, "package.json"), encoding="utf-8") as f:
        pkg = json.load(f)
    manifest = MANIFEST.format(
        name=pkg["name"],
        version=pkg["version"],
        publisher=pkg["publisher"],
        display=pkg["displayName"],
        description=pkg["description"],
        engine=pkg["engines"]["vscode"],
    )
    out = os.path.join(HERE, "{}-{}.vsix".format(pkg["name"], pkg["version"]))
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("extension.vsixmanifest", manifest)
        for name, path in FILES:
            z.write(os.path.join(HERE, path), name)
    print(out)


if __name__ == "__main__":
    main()
