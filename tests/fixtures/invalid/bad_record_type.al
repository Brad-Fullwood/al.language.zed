// Invalid: Record type reference is fine, but missing end for procedure
codeunit 50203 "Bad Record Type"
{
    procedure GetCustomer(CustomerNo: Code[20]) Result: Record Customer
    begin
    // Missing: end;
}
