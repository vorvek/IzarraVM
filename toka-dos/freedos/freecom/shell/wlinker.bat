@echo off
ms2wlink %1 %2 %3 %4 %5 %6 %7 %8 %9 ,,,, > tmp.lnk
echo op map,statics,verbose,stack=4k >> tmp.lnk
rem Toka-DOS: force case-sensitive symbol resolution. Open Watcom 2.0's wlink
rem defaults to case-INSENSITIVE matching, which makes the C-library reference
rem toupper_ resolve to FreeCOM's own toUpper_ (and tolower_->toLower_),
rem producing infinite recursion (toUpper->toMTrans->toupper->toUpper...).
echo op caseexact >> tmp.lnk
call wlink @tmp.lnk
del tmp.lnk
